//! What settling the machine before a boot proof compares its counts found (TASKS item 67):
//! the judgement `sched::settle` makes from what it reads, as plain data tested on the host.
//!
//! A settle is whole when no thread whose end may have been seen is still on its processor (TASKS
//! item 79), no exited thread awaits its reaping, no kernel stack is retired, being reclaimed or
//! waiting after a stall, no closed channel's messages are listed or being freed
//! (TASKS item 81), and no stack was lost while it ran. Nor has any wake-up yet followed a thread
//! that moved, since no thread moves (TASKS item 92): one that did is named by every later
//! settle.
//!
//! A stack waits when its reclaim's shootdown stalls: some processor did not answer within the
//! shootdown's patience (1 s), so a stale translation may remain, and the slot and its frames are
//! withheld until a fresh shootdown completes (TASKS item 73). Under QEMU a processor left
//! unscheduled by a busy host does it; on a real machine a processor held in SMM for over a
//! second would do it too. Each round of the settle's wait reclaims, so a stack whose processor
//! answers within it is given back; one still waiting when the wait ends is named, with the
//! processor it waits for, never left to show later as a bare frame mismatch. A stack is lost
//! only by a leak: dropped while a processor's record still held it.

/// What a settle could not settle in its wait.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Unsettled {
    /// Threads whose end may have been seen that had not yet left their processors (TASKS item
    /// 79). One that never does is a kernel bug: every later settle names it, rather than let a
    /// count be read with what it holds.
    pub ending: u64,
    /// Exited threads still awaiting their reaping on their own processors.
    pub zombies: u64,
    /// Kernel stacks still retired, being reclaimed or waiting after a stall.
    pub retired: usize,
    /// The processor the stack waiting after a stall waits for, if one of `retired` does.
    pub waiting_for: Option<usize>,
    /// Closed channels whose undelivered messages were still listed, or still being freed on
    /// some processor (TASKS item 81).
    pub closed: u64,
    /// Kernel stacks lost while the settle ran.
    pub lost: usize,
    /// Moves of a thread that wake-ups have followed since the boot began (`sched::wake`, TASKS
    /// item 92). None, while no thread moves.
    pub followed: u64,
}

impl core::fmt::Display for Unsettled {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let mut first = true;
        let mut part = |f: &mut core::fmt::Formatter<'_>, text: core::fmt::Arguments<'_>| {
            let sep = if first { "" } else { ", " };
            first = false;
            write!(f, "{sep}{text}")
        };
        if self.ending > 0 {
            part(
                f,
                format_args!("{} ending threads not yet exited", self.ending),
            )?;
        }
        if self.zombies > 0 {
            part(f, format_args!("{} exited threads unreaped", self.zombies))?;
        }
        match (self.retired, self.waiting_for) {
            (0, _) => {}
            (retired, None) => part(f, format_args!("{retired} kernel stacks retired"))?,
            (retired, Some(cpu)) => part(
                f,
                format_args!(
                    "{retired} kernel stacks retired (one withheld since CPU {cpu} did not answer its shootdown)"
                ),
            )?,
        }
        if self.closed > 0 {
            part(
                f,
                format_args!("{} closed channels not yet reaped", self.closed),
            )?;
        }
        if self.lost > 0 {
            part(f, format_args!("{} kernel stacks lost", self.lost))?;
        }
        if self.followed > 0 {
            part(
                f,
                format_args!(
                    "{} moves followed by wake-ups, where no thread moves",
                    self.followed
                ),
            )?;
        }
        Ok(())
    }
}

/// The most passes a settle makes before it judges (the review of TASKS items 75, 77 and 81,
/// R81-2; the coordinating session's ruling).
pub const MAX_PASSES: u32 = 3;

/// Whether a settle that has made `passes` passes makes another: the last one left something
/// pending (`pending`: a closed channel listed or being reaped, a thread ending, or a zombie), or
/// its own drain freed a closed channel (`drained`), which may have woken a thread that has not
/// yet run to its end; and it has made fewer than [`MAX_PASSES`]. Otherwise it judges what it
/// reads.
pub fn again(passes: u32, pending: bool, drained: bool) -> bool {
    (pending || drained) && passes < MAX_PASSES
}

/// The settle's verdict from what it read when its waits ended: `ending` threads not yet off
/// their processors, `zombies` exited threads unreaped, `retired` stacks retired, the processor a
/// stack waiting after a stall waits for (`waiting_for`), `closed` closed channels not yet
/// reaped, the stacks lost before it began (`lost_before`) and after it ended (`lost_after`), and
/// the moves wake-ups have followed since the boot began (`followed`). `waiting_for` only says
/// more of `retired`: read apart from it, it is dropped when `retired` is 0, so the verdict rests
/// on the count.
#[allow(clippy::too_many_arguments)]
pub fn judge(
    ending: u64,
    zombies: u64,
    retired: usize,
    waiting_for: Option<usize>,
    closed: u64,
    lost_before: usize,
    lost_after: usize,
    followed: u64,
) -> Result<(), Unsettled> {
    let pending = Unsettled {
        ending,
        zombies,
        retired,
        waiting_for: waiting_for.filter(|_| retired > 0),
        closed,
        lost: lost_after.saturating_sub(lost_before),
        followed,
    };
    if pending == Unsettled::default() {
        Ok(())
    } else {
        Err(pending)
    }
}

/// What a proof's count line adds when a settle around its counts failed: nothing when both
/// held, otherwise "not settled", where, and what.
pub fn not_settled(
    before: Result<(), Unsettled>,
    after: Result<(), Unsettled>,
) -> alloc::string::String {
    match (before, after) {
        (Ok(()), Ok(())) => alloc::string::String::new(),
        (Err(e), _) => alloc::format!("; not settled before its first count: {e}"),
        (Ok(()), Err(e)) => alloc::format!("; not settled before its compare: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_whole_settle_is_ok() {
        assert_eq!(judge(0, 0, 0, None, 0, 0, 0, 0), Ok(()));
        // Stacks lost before the settle are not its doing.
        assert_eq!(judge(0, 0, 0, None, 0, 3, 3, 0), Ok(()));
        // A waiting stack read after the count found none retired says nothing on its own.
        assert_eq!(judge(0, 0, 0, Some(7), 0, 0, 0, 0), Ok(()));
    }

    #[test]
    fn each_thing_pending_is_named() {
        assert_eq!(
            judge(0, 2, 0, None, 0, 0, 0, 0).unwrap_err().to_string(),
            "2 exited threads unreaped"
        );
        assert_eq!(
            judge(0, 0, 5, None, 0, 0, 0, 0).unwrap_err().to_string(),
            "5 kernel stacks retired"
        );
        // A stack withheld by a stalled shootdown, still waiting for its processor (TASKS item
        // 73): counted among the retired, with the processor named.
        assert_eq!(
            judge(0, 0, 3, Some(7), 0, 0, 0, 0).unwrap_err().to_string(),
            "3 kernel stacks retired (one withheld since CPU 7 did not answer its shootdown)"
        );
        // A loss during the settle: the growth, not the total.
        assert_eq!(
            judge(0, 0, 0, None, 0, 3, 4, 0).unwrap_err().to_string(),
            "1 kernel stacks lost"
        );
        assert_eq!(
            judge(0, 1, 2, None, 0, 0, 3, 0).unwrap_err().to_string(),
            "1 exited threads unreaped, 2 kernel stacks retired, 3 kernel stacks lost"
        );
        // A thread whose end was seen and that has not left its processor (TASKS item 79), named
        // first: alone, and beside everything else.
        assert_eq!(
            judge(1, 0, 0, None, 0, 0, 0, 0).unwrap_err().to_string(),
            "1 ending threads not yet exited"
        );
        assert_eq!(
            judge(2, 1, 2, Some(64), 0, 0, 3, 0)
                .unwrap_err()
                .to_string(),
            "2 ending threads not yet exited, 1 exited threads unreaped, 2 kernel stacks retired (one withheld since CPU 64 did not answer its shootdown), 3 kernel stacks lost"
        );
        // Closed channels not yet reaped (TASKS item 81), after the stacks retired and before
        // the stacks lost: alone, and beside everything else.
        assert_eq!(
            judge(0, 0, 0, None, 1, 0, 0, 0).unwrap_err().to_string(),
            "1 closed channels not yet reaped"
        );
        assert_eq!(
            judge(2, 1, 2, Some(64), 4, 0, 3, 0)
                .unwrap_err()
                .to_string(),
            "2 ending threads not yet exited, 1 exited threads unreaped, 2 kernel stacks retired (one withheld since CPU 64 did not answer its shootdown), 4 closed channels not yet reaped, 3 kernel stacks lost"
        );
        // A wake-up that followed a moved thread, while none moves (TASKS item 92): named last,
        // alone and beside everything else.
        assert_eq!(
            judge(0, 0, 0, None, 0, 0, 0, 1).unwrap_err().to_string(),
            "1 moves followed by wake-ups, where no thread moves"
        );
        assert_eq!(
            judge(0, 1, 0, None, 0, 0, 0, 2).unwrap_err().to_string(),
            "1 exited threads unreaped, 2 moves followed by wake-ups, where no thread moves"
        );
    }

    /// The pass rule (R81-2): a pass that leaves nothing pending and drained nothing itself is
    /// the last; one that leaves something, or whose own drain freed a channel, is followed by
    /// another, up to three in all, after which the settle judges.
    #[test]
    fn a_settle_passes_again_only_while_something_is_pending_or_drained_and_at_most_three_times() {
        assert!(!again(1, false, false), "quiet: judge after one pass");
        assert!(again(1, true, false), "something pending");
        assert!(again(1, false, true), "its own drain freed a channel");
        assert!(again(2, true, true));
        assert!(
            !again(3, true, true),
            "three passes, then judge, which names what is pending"
        );
        assert!(!again(3, false, true), "a drain in the third pass: judged");
        assert!(!again(2, false, false));
        assert_eq!(MAX_PASSES, 3);
        // The passes a settle makes, for each pass's outcome (pending, drained) in turn.
        let passes_made = |outcomes: &[(bool, bool)]| {
            let mut passes = 0;
            for &(pending, drained) in outcomes {
                passes += 1;
                if !again(passes, pending, drained) {
                    break;
                }
            }
            passes
        };
        assert_eq!(passes_made(&[(false, false), (true, true)]), 1);
        assert_eq!(passes_made(&[(false, true), (false, false)]), 2);
        assert_eq!(
            passes_made(&[(true, false), (false, true), (false, false)]),
            3
        );
        assert_eq!(passes_made(&[(true, true); 4]), 3);
    }

    #[test]
    fn a_line_says_where_the_settle_failed() {
        let waiting = judge(0, 0, 1, Some(3), 0, 0, 0, 0);
        assert_eq!(not_settled(Ok(()), Ok(())), "");
        assert_eq!(
            not_settled(waiting, Ok(())),
            "; not settled before its first count: 1 kernel stacks retired (one withheld since CPU 3 did not answer its shootdown)"
        );
        assert_eq!(
            not_settled(Ok(()), waiting),
            "; not settled before its compare: 1 kernel stacks retired (one withheld since CPU 3 did not answer its shootdown)"
        );
    }
}
