//! A failed boot's evidence (TASKS item 165; CLAUDE.md §1, role 26, and §69).
//!
//! When a boot fails, [`keep`] writes what it left into a directory of its own under
//! `target/xtask/failed-boots`:
//! - its serial log, or for a boot with no UART the QEMU monitor's output;
//! - the last screen dump `boot-test` read in that boot, and that dump's rows as text;
//! - COM1's file, where the scenario keeps one;
//! - `boot.txt`, naming the scenario, the machine, the source revision, the verdict, the first
//!   missing marker, the QEMU command line and QEMU's stderr.
//!
//! CI uploads the folder only when a step failed (`.github/workflows/ci.yml`), so a job whose
//! boots all pass has no folder to upload.
//!
//! Nothing here deletes the folder. CI's real-CPU job runs `boot-matrix` and then a
//! `boot-test`, and each can add a directory. A directory is numbered one past the highest
//! number already there, and made with `create_dir`, so none is ever written over.
//!
//! The artifact is public for as long as it is kept. Every text written has the target
//! directory, the repository's root and the home directory replaced by `<target>`, `<repo>` and
//! `~` ([`Prefixes`]). Otherwise the kernel's lines are written as the checker judged them.
//! Keeping the evidence never changes a verdict: the caller says why it was not kept and
//! returns the boot's own error.

use std::borrow::Cow;
use std::fmt::Write as _;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::{Result, screen, serial};

/// The folder the directories go in, under the target directory's `xtask`.
pub(crate) const FOLDER: &str = "failed-boots";

/// The most entries the folder holds. Past this, a failed boot keeps nothing and says so. In
/// CI a job adds at most two, and the cap protects a developer's disk.
pub(crate) const MOST_KEPT: usize = 16;

/// The largest text file written whole. A longer one keeps its first [`TEXT_HEAD`] bytes and its
/// last `TEXT_LIMIT - TEXT_HEAD`, each cut back to whole lines, with a line between them saying
/// how much was cut.
pub(crate) const TEXT_LIMIT: usize = 8 << 20;

/// What a cut text keeps of its start.
pub(crate) const TEXT_HEAD: usize = 1 << 20;

/// The largest screen dump written. That is room for 2560 by 1600 pixels. A larger dump is left
/// out, `boot.txt` says so, and its rows are still written.
pub(crate) const SCREEN_LIMIT: usize = 16 << 20;

/// What a failed boot left, for [`keep`].
pub(crate) struct FailedBoot<'a> {
    pub(crate) label: &'a str,
    /// The `cargo xtask` command that ran it.
    pub(crate) command: String,
    /// The line `boot-matrix` or one of its commands said before the machine, if one did.
    pub(crate) machine: Option<&'a str>,
    /// The machine's settings, as `{:#?}`.
    pub(crate) settings: String,
    pub(crate) loader: String,
    pub(crate) capture: String,
    /// The markers were looked for in screen dumps, not in a serial log.
    pub(crate) judged_on_screen: bool,
    /// QEMU had a monitor that `boot-test` took screen dumps through.
    pub(crate) dumps_screen: bool,
    /// When QEMU was started, in UTC.
    pub(crate) started: String,
    pub(crate) took: Duration,
    pub(crate) timeout: Duration,
    /// QEMU's exit status, as the boot runner read it once it had stopped QEMU.
    pub(crate) qemu_exit: String,
    /// QEMU exited by itself, with a success status, before the boot runner stopped it.
    pub(crate) exited: bool,
    /// The boot's verdict: the failing line, the first missing marker, or the check's message.
    pub(crate) verdict: &'a str,
    pub(crate) markers: &'a [String],
    pub(crate) failures: &'a [&'static str],
    pub(crate) revision: Revision,
    /// QEMU's program and arguments, one item each.
    pub(crate) qemu_command: Vec<String>,
    pub(crate) qemu_stderr: &'a str,
    /// The logs, each with its file name. The one the markers were judged in comes first.
    pub(crate) logs: Vec<(&'static str, String)>,
    /// The last screen dump `boot-test` read in this boot, byte for byte as QEMU wrote it.
    pub(crate) screen: Option<Vec<u8>>,
}

/// The source the boot was built from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Revision {
    /// `git rev-parse HEAD`, or `None` if git could not read it.
    pub(crate) commit: Option<String>,
    /// Whether `git status --porcelain` printed nothing. It is only "clean" or "not clean",
    /// never the list of files, which could name a developer's own files.
    pub(crate) clean: Option<bool>,
}

impl Revision {
    /// The revision of the checkout at `root`.
    pub(crate) fn of(root: &Path) -> Revision {
        let commit = crate::documents::git(root, &["rev-parse", "HEAD"])
            .map(|out| String::from_utf8_lossy(&out).trim().to_string())
            .filter(|commit| !commit.is_empty());
        let clean = commit
            .as_ref()
            .and_then(|_| crate::documents::git(root, &["status", "--porcelain"]))
            .map(|out| out.iter().all(u8::is_ascii_whitespace));
        Revision { commit, clean }
    }

    fn said(&self) -> String {
        match (&self.commit, self.clean) {
            (Some(commit), Some(true)) => format!("commit {commit}, tree clean"),
            (Some(commit), Some(false)) => format!("commit {commit}, tree not clean"),
            (Some(commit), None) => format!("commit {commit}, the tree's state unknown"),
            (None, _) => "unknown: git could not read this checkout".into(),
        }
    }
}

/// The host paths replaced in every text written, longest-lived first: the target directory
/// (which may be inside the repository), then the repository's root, then the home directory.
pub(crate) struct Prefixes(Vec<(String, &'static str)>);

impl Prefixes {
    pub(crate) fn new(target: &Path, repo: &Path, home: Option<&Path>) -> Prefixes {
        let mut pairs = Vec::new();
        for (path, name) in [
            (Some(target), "<target>"),
            (Some(repo), "<repo>"),
            (home, "~"),
        ] {
            let Some(path) = path.and_then(Path::to_str) else {
                continue;
            };
            let path = path.trim_end_matches('/');
            // An empty path, or `/` alone, would replace every slash.
            if path.len() > 1 {
                pairs.push((path.to_string(), name));
            }
        }
        Prefixes(pairs)
    }

    /// This host's: the target directory, the repository's root and `$HOME`.
    pub(crate) fn here() -> Prefixes {
        let home = std::env::var_os("HOME").map(PathBuf::from);
        Prefixes::new(&crate::target_dir(), &crate::repo_root(), home.as_deref())
    }

    pub(crate) fn apply(&self, text: &str) -> String {
        let mut text = text.to_string();
        for (path, name) in &self.0 {
            if text.contains(path.as_str()) {
                text = text.replace(path.as_str(), name);
            }
        }
        text
    }
}

/// Writes `boot`'s evidence to a new directory in `folder` and returns it. It fails, writing
/// nothing, when the folder already holds [`MOST_KEPT`] entries.
pub(crate) fn keep(folder: &Path, boot: &FailedBoot, prefixes: &Prefixes) -> Result<PathBuf> {
    fs::create_dir_all(folder).map_err(|e| format!("{}: {e}", folder.display()))?;
    let names: Vec<String> = fs::read_dir(folder)
        .map_err(|e| format!("{}: {e}", folder.display()))?
        .map(|entry| {
            entry
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .map_err(|e| format!("{}: {e}", folder.display()))
        })
        .collect::<Result<_>>()?;
    if names.len() >= MOST_KEPT {
        return Err(format!(
            "{} failed boots are already kept in {}; remove it",
            names.len(),
            folder.display()
        ));
    }
    let next = names.iter().filter_map(|n| number(n)).max().unwrap_or(0) + 1;
    let dir = new_dir(folder, next, &slug(boot.label))?;

    let rows = boot
        .screen
        .as_deref()
        .map(|ppm| screen::parse_ppm(ppm).and_then(|image| screen::screen_text(&image)));
    let mut texts = vec![("boot.txt", summary(boot, rows.as_ref()))];
    texts.extend(boot.logs.iter().map(|(name, text)| (*name, text.clone())));
    if let Some(rows) = &rows {
        texts.push((
            "screen.txt",
            match rows {
                Ok(rows) => rows.iter().map(|row| format!("{row}\n")).collect(),
                Err(e) => format!("the dump could not be read as the console's screen: {e}\n"),
            },
        ));
    }
    for (name, text) in texts {
        write(&dir.join(name), cut(&prefixes.apply(&text)).as_bytes())?;
    }
    if let Some(ppm) = &boot.screen
        && ppm.len() <= SCREEN_LIMIT
    {
        write(&dir.join("screen.ppm"), ppm)?;
    }
    Ok(dir)
}

/// The number an entry of the folder starts with, if it is `NN-…`.
fn number(name: &str) -> Option<usize> {
    let (digits, _) = name.split_once('-')?;
    digits.parse().ok()
}

/// A new directory `NN-slug` in `folder`, from `first` up: a name already taken is skipped,
/// never written into.
fn new_dir(folder: &Path, first: usize, slug: &str) -> Result<PathBuf> {
    for n in first..first + MOST_KEPT {
        let dir = folder.join(format!("{n:02}-{slug}"));
        match fs::create_dir(&dir) {
            Ok(()) => return Ok(dir),
            Err(e) if e.kind() == ErrorKind::AlreadyExists => {}
            Err(e) => return Err(format!("{}: {e}", dir.display())),
        }
    }
    Err(format!(
        "no free name in {} from {first:02}-{slug} on",
        folder.display()
    ))
}

/// A label as a file name, as `scenario_log` makes it.
fn slug(label: &str) -> String {
    label.replace(|c: char| !c.is_ascii_alphanumeric(), "-")
}

fn write(path: &Path, bytes: &[u8]) -> Result {
    fs::write(path, bytes).map_err(|e| format!("{}: {e}", path.display()))
}

/// `text`, cut to [`TEXT_LIMIT`] if it is longer: its first [`TEXT_HEAD`] bytes and its last
/// `TEXT_LIMIT - TEXT_HEAD`, each cut back to whole lines, and a line between them saying how
/// many bytes were cut.
fn cut(text: &str) -> Cow<'_, str> {
    if text.len() <= TEXT_LIMIT {
        return Cow::Borrowed(text);
    }
    let bytes = text.as_bytes();
    let head = match bytes[..TEXT_HEAD].iter().rposition(|&b| b == b'\n') {
        Some(i) => i + 1,
        None => (0..=TEXT_HEAD)
            .rev()
            .find(|&i| text.is_char_boundary(i))
            .unwrap_or(0),
    };
    let from = text.len() - (TEXT_LIMIT - TEXT_HEAD);
    let tail = match bytes[from..].iter().position(|&b| b == b'\n') {
        Some(i) => from + i + 1,
        None => (from..=text.len())
            .find(|&i| text.is_char_boundary(i))
            .unwrap_or(text.len()),
    };
    Cow::Owned(format!(
        "{}--- xtask: {} bytes cut here (TASKS item 165) ---\n{}",
        &text[..head],
        tail - head,
        &text[tail..]
    ))
}

/// `boot.txt`. QEMU's stderr comes last, so that a cut leaves the rest whole.
fn summary(boot: &FailedBoot, rows: Option<&Result<Vec<String>>>) -> String {
    let mut s = String::new();
    let mut line = |text: String| {
        s.push_str(&text);
        s.push('\n');
    };
    line("A failed boot's evidence (TASKS item 165), written by `cargo xtask`.".into());
    line(String::new());
    line(format!("scenario: {}", boot.label));
    line(format!("command: {}", boot.command));
    line(format!(
        "machine: {}",
        boot.machine
            .unwrap_or("(no machine line: the command boots the machine its settings give)")
    ));
    line(format!("started: {}", boot.started));
    line(format!(
        "took: {:.1} s of its {} s timeout",
        boot.took.as_secs_f64(),
        boot.timeout.as_secs()
    ));
    line(format!("source: {}", boot.revision.said()));
    line(format!("loader: {}", boot.loader));
    line(format!("capture: {}", boot.capture));
    line(format!(
        "QEMU: {}; its status once stopped: {}",
        if boot.exited {
            "exited by itself, with a success status, before it was stopped"
        } else {
            "still running when the boot runner stopped it, or exited with a failure"
        },
        boot.qemu_exit
    ));
    line(String::new());
    line("verdict:".into());
    for text in boot.verdict.lines() {
        line(format!("  {text}"));
    }
    let markers: Vec<&str> = boot.markers.iter().map(String::as_str).collect();
    let (judged, where_) = if boot.judged_on_screen {
        (
            rows.and_then(|rows| rows.as_ref().ok())
                .map(|rows| rows.join("\n")),
            "the screen's rows",
        )
    } else {
        (
            boot.logs.first().map(|(_, text)| text.clone()),
            "the serial log",
        )
    };
    line(match judged {
        Some(text) => match serial::first_missing(&text, &markers) {
            Some(marker) => format!("first marker missing from {where_}: {marker:?}"),
            None => format!("first marker missing from {where_}: none; every marker was seen"),
        },
        None => format!("first marker missing from {where_}: not known; there is nothing to read"),
    });
    line(format!(
        "screen: {}",
        match (&boot.screen, rows) {
            (Some(ppm), Some(rows)) if ppm.len() > SCREEN_LIMIT => format!(
                "the last dump read was {} bytes, over the {} MiB limit, so screen.ppm was not \
                 written; screen.txt holds {}",
                ppm.len(),
                SCREEN_LIMIT >> 20,
                if rows.is_ok() {
                    "its rows"
                } else {
                    "why its rows could not be read"
                }
            ),
            (Some(_), _) =>
                "screen.ppm is the last dump boot-test read in this boot, and screen.txt its rows"
                    .into(),
            (None, _) if boot.dumps_screen =>
                "no dump was taken in this boot before it failed".into(),
            (None, _) => "this capture has no monitor, so no screen was read".into(),
        }
    ));
    let names: Vec<&str> = boot.logs.iter().map(|(name, _)| *name).collect();
    line(format!(
        "logs: {}",
        if names.is_empty() {
            "none".to_string()
        } else {
            names.join(", ")
        }
    ));
    line(String::new());
    line("markers, in order:".into());
    for marker in boot.markers {
        line(format!("  {marker}"));
    }
    line("failure messages:".into());
    for failure in boot.failures {
        line(format!("  {failure}"));
    }
    line(String::new());
    line("settings:".into());
    for text in boot.settings.lines() {
        line(format!("  {text}"));
    }
    line(String::new());
    line("QEMU command line, one argument a line:".into());
    for arg in &boot.qemu_command {
        line(format!("  {arg}"));
    }
    line(String::new());
    line("QEMU's stderr:".into());
    let _ = write!(s, "{}", boot.qemu_stderr);
    if !s.ends_with('\n') {
        s.push('\n');
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A new, empty directory for one test, under the target directory.
    fn scratch(name: &str) -> PathBuf {
        let dir = crate::target_dir()
            .join("xtask")
            .join("evidence-tests")
            .join(name);
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("a scratch directory");
        dir
    }

    fn files(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    fn read(path: &Path) -> String {
        fs::read_to_string(path).unwrap()
    }

    const MARKERS: [&str; 2] = ["origin-kernel: first", "origin-kernel: second"];

    fn markers() -> Vec<String> {
        MARKERS.iter().map(|m| (*m).to_string()).collect()
    }

    /// A failed boot under `repo`, on the machine `boot-matrix` names.
    fn boot<'a>(markers: &'a [String], repo: &str) -> FailedBoot<'a> {
        FailedBoot {
            label: "kernel boot",
            command: "cargo xtask boot-matrix".into(),
            machine: Some("xtask: boot-matrix: intel IOMMU, 2 processor(s)"),
            settings: format!("Settings {{\n    ovmf: Some(\"{repo}/firmware/code.fd\"),\n}}"),
            loader: "Verifying".into(),
            capture: "Com1AndScreen".into(),
            judged_on_screen: false,
            dumps_screen: true,
            started: "2026-11-27 10:00:00 UTC".into(),
            took: Duration::from_millis(12_300),
            timeout: Duration::from_secs(600),
            qemu_exit: "Ok(ExitStatus(unix_wait_status(9)))".into(),
            exited: false,
            verdict: "the serial log shows \"origin-kernel: boot refused\"",
            markers,
            failures: &["origin-kernel: boot refused"],
            revision: Revision {
                commit: Some("0123456789abcdef0123456789abcdef01234567".into()),
                clean: Some(true),
            },
            qemu_command: vec![
                "qemu-system-x86_64".into(),
                "-drive".into(),
                format!("file={repo}/target/xtask/esp.img"),
            ],
            qemu_stderr: "qemu-system-x86_64: a warning\n",
            logs: vec![(
                "serial.log",
                "origin-kernel: first\norigin-kernel: boot refused\n".into(),
            )],
            screen: None,
        }
    }

    fn prefixes(repo: &str) -> Prefixes {
        Prefixes::new(
            Path::new(&format!("{repo}/target")),
            Path::new(repo),
            Some(Path::new("/home/someone")),
        )
    }

    /// Each capture writes its own files and no other: the serial log for a serial capture,
    /// the SPCR UART's and COM1's for an SPCR scenario, and the monitor's output, the dump and
    /// its rows for a boot with no UART.
    #[test]
    fn each_capture_writes_its_files_and_no_other() {
        let folder = scratch("captures");
        let markers = markers();
        let repo = "/work/origin-os";

        let serial = boot(&markers, repo);
        let dir = keep(&folder, &serial, &prefixes(repo)).unwrap();
        assert_eq!(files(&dir), ["boot.txt", "serial.log"]);
        assert_eq!(read(&dir.join("serial.log")), serial.logs[0].1);
        assert!(
            read(&dir.join("boot.txt"))
                .contains("screen: no dump was taken in this boot before it failed")
        );

        let mut spcr = boot(&markers, repo);
        spcr.capture = "SpcrUart".into();
        spcr.dumps_screen = false;
        spcr.logs = vec![
            ("spcr-uart.log", "origin-kernel: first\n".into()),
            ("com1.log", "origin-boot: exiting\n".into()),
        ];
        let dir = keep(&folder, &spcr, &prefixes(repo)).unwrap();
        assert_eq!(files(&dir), ["boot.txt", "com1.log", "spcr-uart.log"]);
        assert_eq!(read(&dir.join("com1.log")), "origin-boot: exiting\n");
        assert!(
            read(&dir.join("boot.txt"))
                .contains("screen: this capture has no monitor, so no screen was read")
        );

        let text = "origin-kernel: first\norigin-kernel: boot refused\n";
        let ppm = screen::tests::console_ppm(text);
        let mut no_uart = boot(&markers, repo);
        no_uart.capture = "Screen".into();
        no_uart.judged_on_screen = true;
        no_uart.logs = vec![(
            "monitor.log",
            format!("(qemu) screendump \"{repo}/target/xtask/screen.ppm\"\n"),
        )];
        no_uart.screen = Some(ppm.clone());
        let dir = keep(&folder, &no_uart, &prefixes(repo)).unwrap();
        assert_eq!(
            files(&dir),
            ["boot.txt", "monitor.log", "screen.ppm", "screen.txt"]
        );
        assert_eq!(fs::read(dir.join("screen.ppm")).unwrap(), ppm);
        assert_eq!(read(&dir.join("screen.txt")), text);
        assert_eq!(
            read(&dir.join("monitor.log")),
            "(qemu) screendump \"<target>/xtask/screen.ppm\"\n"
        );
        let summary = read(&dir.join("boot.txt"));
        // The markers were looked for on the screen, so the missing one is the screen's.
        assert!(
            summary
                .contains("first marker missing from the screen's rows: \"origin-kernel: second\""),
            "{summary}"
        );

        // A dump that is not the console's screen is kept as it is, with the reason.
        let mut garbled = boot(&markers, repo);
        garbled.judged_on_screen = true;
        garbled.screen = Some(b"P6\n2 1\n255\n\0\0\0\0\0\0".to_vec());
        let dir = keep(&folder, &garbled, &prefixes(repo)).unwrap();
        assert!(
            read(&dir.join("screen.txt"))
                .starts_with("the dump could not be read as the console's screen: ")
        );
        assert!(
            read(&dir.join("boot.txt"))
                .contains("first marker missing from the screen's rows: not known")
        );
        assert_eq!(
            files(&folder),
            [
                "01-kernel-boot",
                "02-kernel-boot",
                "03-kernel-boot",
                "04-kernel-boot"
            ]
        );
    }

    /// `boot.txt` names what the contract lists: the scenario, the command, the machine, the
    /// start in UTC, the revision with only "clean" or "not clean", the verdict, the first
    /// missing marker, the settings and the command line with host paths replaced, and QEMU's
    /// stderr last.
    #[test]
    fn the_summary_names_the_boot() {
        let folder = scratch("summary");
        let markers = markers();
        let repo = "/home/someone/work/origin-os";
        let mut failed = boot(&markers, repo);
        failed.revision.clean = Some(false);
        let dir = keep(&folder, &failed, &prefixes(repo)).unwrap();
        let summary = read(&dir.join("boot.txt"));
        for expected in [
            "scenario: kernel boot\n",
            "command: cargo xtask boot-matrix\n",
            "machine: xtask: boot-matrix: intel IOMMU, 2 processor(s)\n",
            "started: 2026-11-27 10:00:00 UTC\n",
            "took: 12.3 s of its 600 s timeout\n",
            "source: commit 0123456789abcdef0123456789abcdef01234567, tree not clean\n",
            "verdict:\n  the serial log shows \"origin-kernel: boot refused\"\n",
            "first marker missing from the serial log: \"origin-kernel: second\"\n",
            "markers, in order:\n  origin-kernel: first\n  origin-kernel: second\n",
            "    ovmf: Some(\"<repo>/firmware/code.fd\"),\n",
            "  file=<target>/xtask/esp.img\n",
        ] {
            assert!(summary.contains(expected), "{expected:?} in\n{summary}");
        }
        assert!(!summary.contains("/home/someone"), "{summary}");
        assert!(
            summary.ends_with("QEMU's stderr:\nqemu-system-x86_64: a warning\n"),
            "{summary}"
        );
        // Without a commit, it says so.
        let unknown = Revision {
            commit: None,
            clean: None,
        };
        assert_eq!(unknown.said(), "unknown: git could not read this checkout");
    }

    /// The next directory is one past the highest number there, whatever else the folder holds;
    /// a name already taken is skipped and never written into.
    #[test]
    fn directories_are_numbered_after_those_already_kept() {
        let folder = scratch("numbering");
        let markers = markers();
        let repo = "/work/origin-os";
        fs::create_dir(folder.join("01-kernel-boot")).unwrap();
        fs::create_dir(folder.join("05-console--no-UART-at-all")).unwrap();
        fs::write(folder.join("notes"), "").unwrap();
        let dir = keep(&folder, &boot(&markers, repo), &prefixes(repo)).unwrap();
        assert_eq!(dir.file_name().unwrap(), "06-kernel-boot");
        assert!(files(&folder.join("05-console--no-UART-at-all")).is_empty());

        let taken = scratch("numbering-taken");
        fs::create_dir(taken.join("02-kernel-boot")).unwrap();
        assert_eq!(
            new_dir(&taken, 2, "kernel-boot")
                .unwrap()
                .file_name()
                .unwrap(),
            "03-kernel-boot"
        );
        assert!(files(&taken.join("02-kernel-boot")).is_empty());
    }

    /// At [`MOST_KEPT`] entries, a failed boot writes nothing and says why.
    #[test]
    fn a_full_folder_keeps_nothing_and_says_so() {
        let folder = scratch("full");
        let markers = markers();
        let repo = "/work/origin-os";
        for n in 1..=MOST_KEPT {
            fs::create_dir(folder.join(format!("{n:02}-kernel-boot"))).unwrap();
        }
        let before = files(&folder);
        let e = keep(&folder, &boot(&markers, repo), &prefixes(repo)).unwrap_err();
        assert!(
            e.starts_with(&format!("{MOST_KEPT} failed boots are already kept in ")),
            "{e}"
        );
        assert!(e.ends_with("; remove it"), "{e}");
        assert_eq!(files(&folder), before);
    }

    /// A text over [`TEXT_LIMIT`] keeps its first and last whole lines within the limit, with
    /// the number of bytes cut between them; a text at the limit is written whole.
    #[test]
    fn a_long_log_keeps_its_start_and_its_end() {
        let line = "origin-kernel: a line of the boot, numbered so the cut can be found 0000\n";
        let lines = TEXT_LIMIT / line.len() + 1000;
        let text: String = (0..lines)
            .map(|n| line.replace("0000", &format!("{n:07}")))
            .collect();
        assert!(text.len() > TEXT_LIMIT);
        let kept = cut(&text);
        let (head, rest) = kept.split_once("--- xtask: ").unwrap();
        let (count, tail) = rest
            .split_once(" bytes cut here (TASKS item 165) ---\n")
            .unwrap();
        assert!(head.len() <= TEXT_HEAD && head.len() > TEXT_HEAD - 200);
        assert!(tail.len() <= TEXT_LIMIT - TEXT_HEAD);
        assert!(tail.len() > TEXT_LIMIT - TEXT_HEAD - 200);
        assert!(text.starts_with(head) && text.ends_with(tail));
        assert!(head.ends_with('\n') && tail.starts_with("origin-kernel: "));
        assert_eq!(
            count.parse::<usize>().unwrap(),
            text.len() - head.len() - tail.len()
        );
        // A text without line ends is cut on character boundaries.
        let wide = "§".repeat(TEXT_LIMIT / 2 + 3);
        let kept = cut(&wide);
        assert!(kept.contains(" bytes cut here "));

        let whole = "x".repeat(TEXT_LIMIT - 1) + "\n";
        assert!(matches!(cut(&whole), Cow::Borrowed(_)));

        let folder = scratch("long");
        let markers = markers();
        let repo = "/work/origin-os";
        let mut failed = boot(&markers, repo);
        failed.logs = vec![("serial.log", text.clone())];
        let dir = keep(&folder, &failed, &prefixes(repo)).unwrap();
        assert_eq!(read(&dir.join("serial.log")), cut(&text));
    }

    /// A dump over [`SCREEN_LIMIT`] is left out, `boot.txt` says so, and its rows are written.
    #[test]
    fn a_dump_over_the_limit_is_left_out() {
        let folder = scratch("screen-limit");
        let markers = markers();
        let repo = "/work/origin-os";
        let mut failed = boot(&markers, repo);
        let mut ppm = b"P6\n1 1\n255\n".to_vec();
        ppm.resize(SCREEN_LIMIT + 1, 0);
        failed.screen = Some(ppm);
        let dir = keep(&folder, &failed, &prefixes(repo)).unwrap();
        assert_eq!(files(&dir), ["boot.txt", "screen.txt", "serial.log"]);
        assert!(read(&dir.join("boot.txt")).contains(&format!(
            "the last dump read was {} bytes, over the 16 MiB limit, so screen.ppm was not written",
            SCREEN_LIMIT + 1
        )));
    }

    /// The target directory is replaced before the repository's root it lies in, and the home
    /// directory last; an empty path or `/` alone is never a prefix.
    #[test]
    fn host_paths_are_replaced() {
        let p = Prefixes::new(
            Path::new("/home/someone/origin-os/target/"),
            Path::new("/home/someone/origin-os"),
            Some(Path::new("/home/someone")),
        );
        assert_eq!(
            p.apply("/home/someone/origin-os/target/xtask/a /home/someone/origin-os/b /home/someone/c /opt/d"),
            "<target>/xtask/a <repo>/b ~/c /opt/d"
        );
        let root = Prefixes::new(
            Path::new("/elsewhere/t"),
            Path::new("/"),
            Some(Path::new("")),
        );
        assert_eq!(root.apply("/a/b /elsewhere/t/x"), "/a/b <target>/x");
    }
}
