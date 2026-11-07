//! A bounded kernel log buffer (CLAUDE.md §69 structured logging and persistent failure
//! records). A ring of fixed-size records — a level and a message — so the most recent
//! lines are always retained and the oldest are dropped when it fills, the behaviour a
//! `dmesg`-style ring and a crash-diagnostics buffer both want. Each record stores its
//! message inline (no per-line allocation), truncated on a UTF-8 boundary, so a logger can
//! run from a context where allocation is undesirable. Built on [`crate::ring`]. Pure, no
//! `unsafe`.

extern crate alloc;

use crate::ring::Ring;

/// The inline message capacity of one record, in bytes.
pub const MSG_CAP: usize = 120;

/// A log level.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Level {
    Debug,
    #[default]
    Info,
    Warn,
    Error,
}

impl Level {
    /// A short tag for display.
    pub fn tag(self) -> &'static str {
        match self {
            Level::Debug => "DEBUG",
            Level::Info => "INFO",
            Level::Warn => "WARN",
            Level::Error => "ERROR",
        }
    }
}

/// One log record: a level and an inline, UTF-8 message truncated to [`MSG_CAP`] bytes.
#[derive(Clone, Copy)]
pub struct Record {
    level: Level,
    len: usize,
    buf: [u8; MSG_CAP],
}

impl Default for Record {
    fn default() -> Self {
        Self {
            level: Level::Info,
            len: 0,
            buf: [0; MSG_CAP],
        }
    }
}

impl Record {
    fn new(level: Level, message: &str) -> Record {
        let mut buf = [0u8; MSG_CAP];
        let mut len = 0;
        // Copy whole scalars until the next would overflow, so the stored bytes stay valid
        // UTF-8 and no character is split.
        for ch in message.chars() {
            let w = ch.len_utf8();
            if len + w > MSG_CAP {
                break;
            }
            ch.encode_utf8(&mut buf[len..len + w]);
            len += w;
        }
        Record { level, len, buf }
    }

    pub fn level(&self) -> Level {
        self.level
    }

    /// The message text (always valid UTF-8; may be truncated).
    pub fn message(&self) -> &str {
        core::str::from_utf8(&self.buf[..self.len]).unwrap_or("")
    }

    /// Whether the message was truncated to fit (its character count differs from the
    /// source is not knowable here; this reports that the buffer is full to the boundary).
    pub fn is_full(&self) -> bool {
        self.len + 4 > MSG_CAP // within one max-width scalar of the cap
    }
}

impl core::fmt::Debug for Record {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "[{}] {}", self.level.tag(), self.message())
    }
}

/// A bounded kernel log: a ring of the most recent [`Record`]s.
#[derive(Debug)]
pub struct Log {
    ring: Ring<Record>,
    dropped: usize,
    total: usize,
}

impl Log {
    /// A log retaining up to `capacity` records.
    pub fn new(capacity: usize) -> Self {
        Self {
            ring: Ring::new(capacity),
            dropped: 0,
            total: 0,
        }
    }

    /// Appends a record, dropping the oldest when full.
    pub fn log(&mut self, level: Level, message: &str) {
        self.total += 1;
        if self.ring.force_push(Record::new(level, message)).is_some() {
            self.dropped += 1;
        }
    }

    /// The number of records currently retained.
    pub fn len(&self) -> usize {
        self.ring.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ring.is_empty()
    }

    /// How many records were dropped because the ring was full.
    pub fn dropped(&self) -> usize {
        self.dropped
    }

    /// How many records have ever been logged.
    pub fn total(&self) -> usize {
        self.total
    }

    /// Drains the retained records oldest-first (consuming them).
    pub fn drain(&mut self) -> impl Iterator<Item = Record> + '_ {
        core::iter::from_fn(move || self.ring.pop())
    }
}

#[cfg(test)]
mod tests {
    extern crate alloc;
    use super::*;
    use alloc::string::String;
    use alloc::vec::Vec;

    #[test]
    fn retains_recent_and_drops_oldest() {
        let mut log = Log::new(4);
        for i in 0..4 {
            log.log(Level::Info, &alloc::format!("line {i}"));
        }
        assert_eq!(log.len(), 4);
        assert_eq!(log.dropped(), 0);
        // Two more push the two oldest out.
        log.log(Level::Warn, "line 4");
        log.log(Level::Error, "line 5");
        assert_eq!(log.len(), 4);
        assert_eq!(log.dropped(), 2);
        assert_eq!(log.total(), 6);
        let kept: Vec<String> = log.drain().map(|r| r.message().into()).collect();
        assert_eq!(
            kept,
            ["line 2", "line 3", "line 4", "line 5"],
            "newest four, in order"
        );
    }

    #[test]
    fn levels_and_message_are_recorded() {
        let mut log = Log::new(8);
        log.log(Level::Error, "disk failed");
        log.log(Level::Debug, "probe ok");
        let records: Vec<(Level, String)> = log
            .drain()
            .map(|r| (r.level(), r.message().into()))
            .collect();
        assert_eq!(records[0], (Level::Error, "disk failed".into()));
        assert_eq!(records[1], (Level::Debug, "probe ok".into()));
        assert_eq!(Level::Error.tag(), "ERROR");
        assert_eq!(Level::default(), Level::Info);
    }

    #[test]
    fn a_long_message_is_truncated_on_a_char_boundary() {
        // A message of multi-byte scalars longer than MSG_CAP must be cut cleanly.
        let long: String = "é".repeat(MSG_CAP); // 2 bytes each
        let mut log = Log::new(2);
        log.log(Level::Info, &long);
        let rec = log.drain().next().unwrap();
        let msg = rec.message();
        assert!(msg.len() <= MSG_CAP);
        assert!(
            msg.is_char_boundary(msg.len()),
            "valid UTF-8, no split scalar"
        );
        assert!(msg.chars().all(|c| c == 'é'));
        // MSG_CAP / 2 whole 'é' scalars fit (120 / 2 = 60).
        assert_eq!(msg.chars().count(), MSG_CAP / 2);
    }

    #[test]
    fn capacity_one_keeps_only_the_latest() {
        let mut log = Log::new(1);
        log.log(Level::Info, "a");
        log.log(Level::Info, "b");
        log.log(Level::Info, "c");
        assert_eq!(log.dropped(), 2);
        let kept: Vec<String> = log.drain().map(|r| r.message().into()).collect();
        assert_eq!(kept, ["c"]);
    }
}
