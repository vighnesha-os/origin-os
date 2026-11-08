//! Extracting a ustar archive into the in-memory VFS (CLAUDE.md §21 initial system
//! deployment). This is the bootstrap step that turns an initial RAM filesystem image (a
//! tar archive, [`crate::ustar`]) into a live file namespace ([`crate::vfs`]) the runtime
//! and services can read. Directories are created as needed (even when the archive does not
//! list a parent), files are written with their contents, and other entry types (symlinks,
//! devices) are skipped for now.
//!
//! Archive names are untrusted. Extraction refuses an absolute name or any name with a `..`
//! component, so a crafted archive cannot write outside the target tree — the classic
//! tar path-traversal defense (§34, §58). Pure, no `unsafe`.

extern crate alloc;
use alloc::string::String;

use crate::ustar;
use crate::vfs::{self, Vfs};

/// Why extraction stopped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// The archive itself was malformed.
    Archive(ustar::UstarError),
    /// A VFS operation failed.
    Vfs(vfs::FsError),
    /// An entry name was absolute or escaped the tree with `..` (a traversal attempt).
    UnsafePath,
}

impl From<ustar::UstarError> for Error {
    fn from(e: ustar::UstarError) -> Self {
        Error::Archive(e)
    }
}
impl From<vfs::FsError> for Error {
    fn from(e: vfs::FsError) -> Self {
        Error::Vfs(e)
    }
}

/// Turns a relative archive name into a safe absolute VFS path, or `None` if it is absolute,
/// empty, or contains a `.`/`..`/empty component. A trailing slash (directory form) is
/// trimmed.
fn safe_path(name: &str) -> Option<String> {
    let name = name.trim_end_matches('/');
    if name.is_empty() || name.starts_with('/') {
        return None;
    }
    let mut path = String::new();
    for component in name.split('/') {
        if component.is_empty() || component == "." || component == ".." {
            return None; // no empty, current or parent components
        }
        path.push('/');
        path.push_str(component);
    }
    (!path.is_empty()).then_some(path)
}

/// The parent directory of an absolute path (`/a/b` → `/a`, `/a` → `/`).
fn parent(path: &str) -> &str {
    match path.rfind('/') {
        Some(0) | None => "/",
        Some(i) => &path[..i],
    }
}

/// Extracts `archive` into `vfs`, returning how many entries were applied (files and
/// directories; skipped entry types do not count). A name that is absolute or uses `..` is
/// refused as [`Error::UnsafePath`] before anything is written for it.
pub fn extract(archive: &[u8], vfs: &mut Vfs) -> Result<usize, Error> {
    let mut applied = 0;
    for entry in ustar::parse(archive)? {
        let path = match safe_path(&entry.name) {
            Some(p) => p,
            // A directory entry named "." / "" is a harmless no-op; a "../…" is an attack.
            None if entry.name.trim_end_matches('/').is_empty() => continue,
            None => return Err(Error::UnsafePath),
        };

        if entry.is_dir() {
            vfs.mkdir_all(&path)?;
        } else if entry.is_file() {
            vfs.mkdir_all(parent(&path))?;
            // Create, or reuse and truncate an entry the archive lists twice.
            match vfs.create(&path) {
                Ok(_) => {}
                Err(vfs::FsError::Exists) => vfs.truncate(&path, 0)?,
                Err(e) => return Err(Error::Vfs(e)),
            }
            if !entry.data.is_empty() {
                vfs.write(&path, 0, entry.data)?;
            }
        } else {
            continue; // symlinks, hard links, devices: skipped in v0
        }
        applied += 1;
    }
    Ok(applied)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ustar::{BLOCK, typeflag};
    use alloc::vec::Vec;

    /// A minimal ustar builder (mirrors the one in the `ustar` tests).
    fn ustar_archive(entries: &[(&str, u8, &[u8])]) -> Vec<u8> {
        let mut buf = Vec::new();
        for &(name, flag, data) in entries {
            let mut h = [0u8; BLOCK];
            h[0..name.len()].copy_from_slice(name.as_bytes());
            h[156] = flag;
            h[257..263].copy_from_slice(b"ustar\0");
            h[263..265].copy_from_slice(b"00");
            let size = alloc::format!("{:011o}", data.len());
            h[124..135].copy_from_slice(size.as_bytes());
            let sum: u32 = h
                .iter()
                .enumerate()
                .map(|(i, &b)| {
                    if (148..156).contains(&i) {
                        32
                    } else {
                        u32::from(b)
                    }
                })
                .sum();
            let chk = alloc::format!("{sum:06o}\0 ");
            h[148..156].copy_from_slice(chk.as_bytes());
            buf.extend_from_slice(&h);
            buf.extend_from_slice(data);
            let pad = (BLOCK - data.len() % BLOCK) % BLOCK;
            buf.extend_from_slice(&alloc::vec![0u8; pad]);
        }
        buf.extend_from_slice(&[0u8; BLOCK * 2]);
        buf
    }

    fn read(vfs: &Vfs, path: &str) -> Vec<u8> {
        let mut out = alloc::vec![0u8; 256];
        let n = vfs.read(path, 0, &mut out).unwrap();
        out.truncate(n);
        out
    }

    #[test]
    fn extracts_a_tree_into_the_vfs() {
        let archive = ustar_archive(&[
            ("hello.txt", typeflag::REGULAR, b"hi\n"),
            ("etc/", typeflag::DIRECTORY, b""),
            ("etc/hostname", typeflag::REGULAR, b"origin\n"),
            // A file whose parent directories are not listed in the archive.
            ("usr/bin/tool", typeflag::REGULAR, b"ELF..."),
        ]);
        let mut vfs = Vfs::new(1 << 20, 1000);
        let n = extract(&archive, &mut vfs).unwrap();
        assert_eq!(n, 4);

        assert_eq!(read(&vfs, "/hello.txt"), b"hi\n");
        assert_eq!(read(&vfs, "/etc/hostname"), b"origin\n");
        assert_eq!(read(&vfs, "/usr/bin/tool"), b"ELF...");
        // The implicit parents exist as directories.
        assert!(vfs.list("/usr").unwrap().contains(&String::from("bin")));
        assert!(
            vfs.list("/etc")
                .unwrap()
                .contains(&String::from("hostname"))
        );
    }

    #[test]
    fn refuses_path_traversal_and_absolute_names() {
        for bad in ["../evil", "a/../../evil", "/etc/passwd", "a/./b"] {
            let archive = ustar_archive(&[(bad, typeflag::REGULAR, b"x")]);
            let mut vfs = Vfs::new(1 << 16, 100);
            assert_eq!(
                extract(&archive, &mut vfs),
                Err(Error::UnsafePath),
                "{bad:?} must be refused"
            );
        }
        // A malformed archive surfaces as Error::Archive, not a panic.
        let mut vfs = Vfs::new(1 << 16, 100);
        let mut junk = ustar_archive(&[("f", typeflag::REGULAR, b"x")]);
        junk[148] ^= 0xFF; // corrupt the checksum
        assert!(matches!(extract(&junk, &mut vfs), Err(Error::Archive(_))));
    }

    /// Mutated archives extract into a bounded tree or are refused; a name that leaves the
    /// tree is never applied, and the tree that results has only clean paths (CLAUDE.md §72).
    #[test]
    fn mutated_archives_extract_safely_or_not_at_all() {
        use crate::testing::{Rng, mutate, mutate_window};
        let base = ustar_archive(&[
            ("hello.txt", typeflag::REGULAR, b"hi\n"),
            ("etc/", typeflag::DIRECTORY, b""),
            ("etc/hostname", typeflag::REGULAR, b"origin\n"),
            ("usr/bin/tool", typeflag::REGULAR, &alloc::vec![9u8; 900]),
            ("a/b", typeflag::SYMLINK, b""),
        ]);
        let reseal = |h: &mut [u8]| {
            h[148..156].fill(b' ');
            let sum: u32 = h[..BLOCK].iter().map(|&b| u32::from(b)).sum();
            let chk = alloc::format!("{sum:06o}\0 ");
            h[148..156].copy_from_slice(chk.as_bytes());
        };
        let mut rng = Rng::new(0x4E46_5352);
        let (mut extracted, mut refused) = (0u32, 0u32);
        for case in 0..crate::testing::cases(6000) {
            let mut data = base.clone();
            if case % 2 == 0 {
                mutate(&mut data, &mut rng);
            } else {
                // A mutated name, re-sealed so the name is what extraction must judge.
                let header = BLOCK * rng.below(4).min(3);
                if header + BLOCK <= data.len() {
                    mutate_window(&mut data, header..header + 100, &mut rng);
                    reseal(&mut data[header..header + BLOCK]);
                }
            }
            let mut vfs = Vfs::new(4096, 64);
            let archive = ustar::parse(&data);
            match extract(&data, &mut vfs) {
                Ok(n) => {
                    extracted += 1;
                    let entries = archive.unwrap();
                    assert!(n <= entries.len(), "case {case}");
                    // Everything applied has a clean path under the root.
                    for e in entries.iter().filter(|e| e.is_file() || e.is_dir()) {
                        let name = e.name.trim_end_matches('/');
                        if name.is_empty() {
                            continue;
                        }
                        assert!(!name.starts_with('/'), "case {case}: {name:?}");
                        assert!(
                            name.split('/')
                                .all(|c| !c.is_empty() && c != "." && c != ".."),
                            "case {case}: {name:?}"
                        );
                        assert!(
                            vfs.resolve(&alloc::format!("/{name}")).is_ok(),
                            "case {case}"
                        );
                    }
                }
                Err(Error::UnsafePath) => {
                    refused += 1;
                    assert!(archive.is_ok(), "case {case}");
                }
                Err(_) => refused += 1,
            }
        }
        assert!(extracted > 1000 && refused > 1000, "{extracted}/{refused}");
    }

    #[test]
    fn a_duplicate_entry_is_overwritten_not_appended() {
        let archive = ustar_archive(&[
            ("note", typeflag::REGULAR, b"first version, longer"),
            ("note", typeflag::REGULAR, b"second"),
        ]);
        let mut vfs = Vfs::new(1 << 16, 100);
        extract(&archive, &mut vfs).unwrap();
        assert_eq!(read(&vfs, "/note"), b"second", "truncated, no stale tail");
    }
}
