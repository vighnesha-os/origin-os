//! Heavy runs take turns: one at a time across every checkout of this repository on one
//! machine (the owner's decision of 2026-11-10).
//!
//! Each worktree builds into a target directory of its own, so two sessions' builds and QEMU
//! boots used to overlap. On a 10-core host that reached a load of 70, and the proofs that time
//! one clock against another then fail under QEMU's software CPU with nothing wrong in the
//! kernel. So `check`, the boots, `loom`, `build` and `run` take an exclusive lock on one file
//! in the repository's common git directory, which every worktree of the repository shares.
//!
//! - **No lock can be left behind.** It is `flock` (`std::fs::File::lock`), which the operating
//!   system releases when the process ends, however it ends. Linux and macOS alike.
//! - **A run's own children do not wait for it.** `boot-matrix` runs `xtask boot-test` as
//!   child processes. The holder gives each child [`HELD`] with its process ID, and a child that
//!   finds the lock held by exactly that process goes on inside its parent's turn. A stale or
//!   copied variable names some other process, so that child takes its turn like anyone else.
//! - **A waiter says what it is waiting for.** The holder writes its process ID, worktree,
//!   command and start time into the lock file, and a waiter prints them once, then about once
//!   a minute.
//!
//! On CI each job has its runner to itself, so the lock is never contended there.

use std::fmt::Write as _;
use std::fs::{File, OpenOptions, TryLockError};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::{Result, repo_root};

/// What a holder gives its child processes: its process ID. A child that finds the lock held
/// by that process runs inside its parent's turn.
pub(crate) const HELD: &str = "ORIGIN_XTASK_LOCK_HELD";
/// The lock file, in the repository's common git directory.
const FILE_NAME: &str = "origin-xtask-turn.lock";
/// How often a waiter says that it is still waiting.
const REPORT_EVERY: Duration = Duration::from_secs(60);
/// How often a waiter tries the lock again.
const POLL: Duration = Duration::from_millis(200);

/// The process whose turn this process runs in, once it has one: itself, or the parent that
/// passed [`HELD`]. What [`pass`] hands on.
static TURN_OF: OnceLock<u32> = OnceLock::new();

/// A turn on the machine: the lock is held until this is dropped, or until the process ends.
#[derive(Debug)]
pub(crate) struct Turn {
    /// The open lock file, if this process holds the lock itself; `None` inside a parent's turn.
    file: Option<File>,
}

impl Drop for Turn {
    fn drop(&mut self) {
        if let Some(file) = &self.file {
            // The record goes first, so that nobody reads it as current once the lock is free.
            let _ = file.set_len(0);
            let _ = file.unlock();
        }
    }
}

/// Who holds the lock, as the holder wrote it into the file.
#[derive(Debug, Default, PartialEq, Eq)]
struct Holder {
    pid: Option<u32>,
    worktree: String,
    command: String,
    /// Seconds since the Unix epoch.
    started: Option<u64>,
}

impl Holder {
    /// This process, about to run `command` in this worktree.
    fn this(command: &str) -> Self {
        Self {
            pid: Some(std::process::id()),
            worktree: repo_root().display().to_string(),
            command: command.to_string(),
            started: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .ok()
                .map(|d| d.as_secs()),
        }
    }

    /// The record, a line per field.
    fn record(&self) -> String {
        let mut text = String::new();
        if let Some(pid) = self.pid {
            let _ = writeln!(text, "pid {pid}");
        }
        let _ = writeln!(text, "worktree {}", self.worktree);
        let _ = writeln!(text, "command {}", self.command);
        if let Some(started) = self.started {
            let _ = writeln!(text, "started {started}");
        }
        text
    }

    /// What a record says; fields it does not have stay empty.
    fn parse(text: &str) -> Self {
        let mut holder = Self::default();
        for line in text.lines() {
            let (key, value) = line.split_once(' ').unwrap_or((line, ""));
            match key {
                "pid" => holder.pid = value.parse().ok(),
                "worktree" => holder.worktree = value.to_string(),
                "command" => holder.command = value.to_string(),
                "started" => holder.started = value.parse().ok(),
                _ => {}
            }
        }
        holder
    }

    /// The record in `file`, read from its start.
    fn read(file: &mut File) -> Self {
        let mut text = String::new();
        if file.seek(SeekFrom::Start(0)).is_err() || file.read_to_string(&mut text).is_err() {
            return Self::default();
        }
        Self::parse(&text)
    }

    /// One line for a waiter to print; `now` is seconds since the Unix epoch.
    fn describe(&self, now: u64) -> String {
        let Some(pid) = self.pid else {
            return "another run, which has not said who it is yet".to_string();
        };
        let since = self.started.map_or(String::new(), |started| {
            format!(
                ", since {} ({} min ago)",
                utc(started),
                now.saturating_sub(started) / 60
            )
        });
        format!(
            "process {pid}, `cargo xtask {}` in {}{since}",
            self.command, self.worktree
        )
    }
}

/// `seconds` since the Unix epoch as a UTC date and time, `YYYY-MM-DD HH:MM:SS UTC`.
fn utc(seconds: u64) -> String {
    let (mut days, rest) = (seconds / 86_400, seconds % 86_400);
    let leap = |year: u64| {
        (year.is_multiple_of(4) && !year.is_multiple_of(100)) || year.is_multiple_of(400)
    };
    let mut year = 1970;
    loop {
        let length = if leap(year) { 366 } else { 365 };
        if days < length {
            break;
        }
        days -= length;
        year += 1;
    }
    let february = if leap(year) { 29 } else { 28 };
    let months = [31, february, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    let mut month = 1;
    for length in months {
        if days < length {
            break;
        }
        days -= length;
        month += 1;
    }
    format!(
        "{year:04}-{month:02}-{:02} {:02}:{:02}:{:02} UTC",
        days + 1,
        rest / 3600,
        rest % 3600 / 60,
        rest % 60
    )
}

/// Seconds since the Unix epoch, now.
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// The lock file every checkout of this repository shares: in the common git directory, or,
/// in a copy that is not a git checkout, in this checkout's own target directory.
fn lock_path() -> PathBuf {
    let root = repo_root();
    let common = Command::new("git")
        .args(["rev-parse", "--git-common-dir"])
        .current_dir(&root)
        .output()
        .ok()
        .filter(|out| out.status.success())
        .and_then(|out| String::from_utf8(out.stdout).ok())
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty());
    match common {
        Some(dir) => root.join(dir).join(FILE_NAME),
        None => crate::target_dir().join(FILE_NAME),
    }
}

/// Takes this process's turn on the machine for `command`, waiting for it if another run
/// has it, or goes on inside its parent's turn (see the module documentation).
pub(crate) fn take(command: &str) -> Result<Turn> {
    let inherited = std::env::var(HELD).ok();
    take_at(
        &lock_path(),
        command,
        inherited.as_deref(),
        &mut std::io::stdout(),
    )
}

/// [`take`], with the lock file, the inherited variable and the output given.
fn take_at(
    path: &Path,
    command: &str,
    inherited: Option<&str>,
    out: &mut dyn Write,
) -> Result<Turn> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    match file.try_lock() {
        Ok(()) => {}
        Err(TryLockError::WouldBlock) => {
            // Inside the turn of the process that holds it: a child of that run.
            let parent = inherited.and_then(|value| value.parse::<u32>().ok());
            if let Some(parent) = parent
                && Holder::read(&mut file).pid == Some(parent)
            {
                let _ = TURN_OF.set(parent);
                return Ok(Turn { file: None });
            }
            wait(&mut file, out).map_err(|e| format!("{}: {e}", path.display()))?;
        }
        Err(TryLockError::Error(e)) => return Err(format!("{}: {e}", path.display())),
    }
    let record = Holder::this(command).record();
    file.set_len(0)
        .and_then(|()| file.seek(SeekFrom::Start(0)))
        .and_then(|_| file.write_all(record.as_bytes()))
        .and_then(|()| file.flush())
        .map_err(|e| format!("{}: {e}", path.display()))?;
    let _ = TURN_OF.set(std::process::id());
    Ok(Turn { file: Some(file) })
}

/// Waits for the lock, saying who has it once and then about once a minute.
fn wait(file: &mut File, out: &mut dyn Write) -> std::io::Result<()> {
    let started = Instant::now();
    let mut last_said: Option<Instant> = None;
    loop {
        match file.try_lock() {
            Ok(()) => {
                if last_said.is_some() {
                    let _ = writeln!(
                        out,
                        "xtask: the machine is free after {} s of waiting: going on",
                        started.elapsed().as_secs()
                    );
                }
                return Ok(());
            }
            Err(TryLockError::WouldBlock) => {}
            Err(TryLockError::Error(e)) => return Err(e),
        }
        if last_said.is_none_or(|said| said.elapsed() >= REPORT_EVERY) {
            let holder = Holder::read(file);
            let _ = writeln!(
                out,
                "xtask: {} for the machine: {}",
                if last_said.is_none() {
                    "waiting".to_string()
                } else {
                    format!("still waiting, {} min,", started.elapsed().as_secs() / 60)
                },
                holder.describe(now())
            );
            let _ = out.flush();
            last_said = Some(Instant::now());
        }
        thread::sleep(POLL);
    }
}

/// Tells `command` that it runs inside this process's turn, if it has one: for the xtask
/// children of a heavy run.
pub(crate) fn pass(command: &mut Command) -> &mut Command {
    if let Some(pid) = TURN_OF.get() {
        command.env(HELD, pid.to_string());
    }
    command
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::BufRead;
    use std::sync::mpsc;

    /// A lock file of its own for one test, under the target directory.
    fn lock_file(name: &str) -> PathBuf {
        let dir = crate::target_dir().join("xtask").join("turn-tests");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        let _ = std::fs::remove_file(&path);
        path
    }

    #[test]
    fn a_record_says_who_holds_the_lock_and_reads_back_the_same() {
        let holder = Holder {
            pid: Some(4242),
            worktree: "/work/origin os".to_string(),
            command: "boot-matrix".to_string(),
            started: Some(1_794_200_000),
        };
        assert_eq!(Holder::parse(&holder.record()), holder);
        assert_eq!(
            holder.describe(1_794_200_000 + 125),
            "process 4242, `cargo xtask boot-matrix` in /work/origin os, since 2026-11-09 04:53:20 UTC (2 min ago)"
        );
        // A holder that has not written its record yet, or wrote half of it.
        assert_eq!(Holder::parse(""), Holder::default());
        assert!(Holder::parse("").describe(0).contains("not said who it is"));
        assert_eq!(Holder::parse("pid 7\nworktree").pid, Some(7));
    }

    #[test]
    fn utc_dates_are_right_across_leap_years_and_month_ends() {
        assert_eq!(utc(0), "1970-01-01 00:00:00 UTC");
        assert_eq!(utc(951_782_399), "2000-02-28 23:59:59 UTC");
        assert_eq!(utc(951_782_400), "2000-02-29 00:00:00 UTC");
        assert_eq!(utc(1_709_251_199), "2024-02-29 23:59:59 UTC");
        assert_eq!(utc(4_107_542_400), "2100-03-01 00:00:00 UTC");
        assert_eq!(utc(1_798_761_599), "2026-12-31 23:59:59 UTC");
    }

    #[test]
    fn a_second_taker_waits_says_whom_for_and_goes_on_when_the_first_is_done() {
        let path = lock_file("second-waits.lock");
        let first = take_at(&path, "boot-matrix", None, &mut Vec::new()).unwrap();
        let (done, finished) = mpsc::channel();
        let waiter = {
            let path = path.clone();
            thread::spawn(move || {
                let mut said = Vec::new();
                let turn = take_at(&path, "check", None, &mut said).unwrap();
                done.send(()).unwrap();
                (turn, String::from_utf8(said).unwrap())
            })
        };
        assert!(
            finished.recv_timeout(Duration::from_millis(800)).is_err(),
            "the second taker went on while the first held the lock"
        );
        drop(first);
        finished
            .recv_timeout(Duration::from_secs(5))
            .expect("the second taker never got its turn");
        let (_turn, said) = waiter.join().unwrap();
        let me = std::process::id();
        assert!(
            said.starts_with(&format!(
                "xtask: waiting for the machine: process {me}, `cargo xtask boot-matrix` in "
            )),
            "{said}"
        );
        assert!(said.contains("the machine is free after"), "{said}");
        // The new holder's record replaced the old one.
        let mut file = File::open(&path).unwrap();
        assert_eq!(Holder::read(&mut file).command, "check");
    }

    #[test]
    fn the_lock_goes_with_the_turn() {
        let path = lock_file("goes-with-the-turn.lock");
        drop(take_at(&path, "loom", None, &mut Vec::new()).unwrap());
        let mut said = Vec::new();
        let _again = take_at(&path, "loom", None, &mut said).unwrap();
        assert!(said.is_empty(), "a free lock was waited for");
    }

    /// The child process of the next test: takes the lock named by the environment, says so,
    /// and waits to be killed.
    #[test]
    #[ignore = "a helper the next test runs in a process of its own"]
    fn hold_the_lock_until_killed() {
        let Some(path) = std::env::var_os("ORIGIN_TURN_TEST_LOCK") else {
            return;
        };
        let _turn = take_at(Path::new(&path), "boot-test", None, &mut Vec::new()).unwrap();
        println!("holding");
        std::io::stdout().flush().unwrap();
        thread::sleep(Duration::from_secs(120));
    }

    #[test]
    fn the_lock_goes_with_the_process_however_it_ends() {
        let path = lock_file("goes-with-the-process.lock");
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "turns::tests::hold_the_lock_until_killed",
                "--exact",
                "--ignored",
                "--nocapture",
                "--test-threads=1",
            ])
            .env("ORIGIN_TURN_TEST_LOCK", &path)
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let stdout = child.stdout.take().unwrap();
        let holding = std::io::BufReader::new(stdout)
            .lines()
            .map_while(std::result::Result::ok)
            .any(|line| line.contains("holding"));
        assert!(holding, "the child never took the lock");
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        assert!(
            matches!(file.try_lock(), Err(TryLockError::WouldBlock)),
            "the child's lock was not held"
        );
        // Killed: no destructor runs, and the system still lets go of the lock.
        child.kill().unwrap();
        child.wait().unwrap();
        let mut said = Vec::new();
        let _turn = take_at(&path, "boot-test", None, &mut said).unwrap();
        assert!(said.is_empty(), "the dead process's lock was waited for");
    }

    #[test]
    fn a_child_of_the_holder_runs_in_its_turn_and_any_other_waits() {
        let path = lock_file("children.lock");
        let holder = take_at(&path, "boot-matrix", None, &mut Vec::new()).unwrap();
        let me = std::process::id().to_string();
        // A child of the holder: no wait, and no second lock.
        let mut said = Vec::new();
        let inside = take_at(&path, "boot-test", Some(&me), &mut said).unwrap();
        assert!(inside.file.is_none() && said.is_empty());
        drop(inside);
        // Dropping the child's turn left the holder's lock alone.
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        assert!(matches!(file.try_lock(), Err(TryLockError::WouldBlock)));
        drop(file);
        drop(holder);
        // A variable that names another process, or nothing, waits like everyone else.
        for stale in ["1", "not a pid"] {
            let holder = take_at(&path, "boot-matrix", None, &mut Vec::new()).unwrap();
            let (done, finished) = mpsc::channel();
            let waiter = {
                let path = path.clone();
                thread::spawn(move || {
                    let turn = take_at(&path, "boot-test", Some(stale), &mut Vec::new());
                    done.send(()).unwrap();
                    turn
                })
            };
            assert!(
                finished.recv_timeout(Duration::from_millis(500)).is_err(),
                "a child with {HELD}={stale} did not wait"
            );
            drop(holder);
            finished.recv_timeout(Duration::from_secs(5)).unwrap();
            let turn = waiter.join().unwrap().unwrap();
            assert!(turn.file.is_some(), "it took the lock itself");
        }
    }

    #[test]
    fn a_turn_is_passed_to_children_as_its_holders_process_id() {
        let path = lock_file("passed-on.lock");
        let _turn = take_at(&path, "boot-matrix", None, &mut Vec::new()).unwrap();
        let mut command = Command::new("true");
        pass(&mut command);
        let passed: Vec<_> = command
            .get_envs()
            .filter(|(name, _)| *name == HELD)
            .filter_map(|(_, value)| value.map(|v| v.to_string_lossy().into_owned()))
            .collect();
        assert_eq!(passed, [TURN_OF.get().unwrap().to_string()]);
    }
}
