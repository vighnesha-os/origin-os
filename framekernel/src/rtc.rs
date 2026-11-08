//! The CMOS real-time clock's date and time (CLAUDE.md §68: wall clock, calendar), decoded
//! from its registers, and civil-time arithmetic in UTC.
//!
//! Reference: the MC146818-compatible RTC that PC chipsets implement (Intel PCH
//! datasheets, "Real Time Clock (RTC)", the standard bank's registers 00h–0Dh): seconds
//! 00h, minutes 02h, hours 04h, day of month 07h, month 08h, year 09h, register A 0Ah
//! (bit 7: update in progress) and register B 0Bh (bit 1: 24-hour mode; bit 2: binary
//! rather than BCD). The century lives in the CMOS byte that the ACPI FADT's `CENTURY`
//! field names (ACPI 6.6 §5.2.9), when it names one.
//!
//! The RTC keeps UTC on Origin systems: the firmware's time is set in UTC, and time zones
//! are applied above the kernel (CLAUDE.md §68). The registers are untrusted input: every
//! field is range-checked, and an invalid BCD digit is an error, not a guess.
//!
//! [`UpdateWatcher`] and [`SecondMeter`] time the clock's second against a counter: the
//! pure logic of the boot proof that an independent oscillator confirms the TSC's
//! calibration (the kernel binary's `time` module does the port I/O).

use core::fmt;

/// CMOS register indexes.
pub mod register {
    pub const SECONDS: u8 = 0x00;
    pub const MINUTES: u8 = 0x02;
    pub const HOURS: u8 = 0x04;
    pub const DAY: u8 = 0x07;
    pub const MONTH: u8 = 0x08;
    pub const YEAR: u8 = 0x09;
    pub const STATUS_A: u8 = 0x0A;
    pub const STATUS_B: u8 = 0x0B;
}

/// Register A bit 7: an update cycle is in progress (or begins within 244 µs).
pub const UPDATE_IN_PROGRESS: u8 = 0x80;
/// Register B bit 1: 24-hour mode (else 12-hour, with bit 7 of the hours meaning PM).
pub const MODE_24_HOUR: u8 = 0x02;
/// Register B bit 2: binary values (else BCD).
pub const MODE_BINARY: u8 = 0x04;

/// One read of the clock registers.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Registers {
    pub seconds: u8,
    pub minutes: u8,
    pub hours: u8,
    pub day: u8,
    pub month: u8,
    pub year: u8,
    /// The century register's value, if the FADT names one.
    pub century: Option<u8>,
    pub status_b: u8,
}

/// The field a decoding error is about.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Field {
    Second,
    Minute,
    Hour,
    Day,
    Month,
    Year,
    Century,
}

/// A date and time in UTC, at one-second resolution.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct DateTime {
    pub year: u16,
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
}

/// A BCD byte's value, or `None` if a digit is above 9.
const fn from_bcd(v: u8) -> Option<u8> {
    let (hi, lo) = (v >> 4, v & 0x0F);
    if hi > 9 || lo > 9 {
        None
    } else {
        Some(hi * 10 + lo)
    }
}

/// Whether `year` is a Gregorian leap year.
pub const fn is_leap(year: u64) -> bool {
    year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400))
}

/// Days in `month` (1–12) of `year`, or 0 for a month outside 1–12.
pub const fn days_in_month(year: u64, month: u8) -> u8 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap(year) => 29,
        2 => 28,
        _ => 0,
    }
}

/// Days before month `month` (1–12) in a year; February's leap day counts when `leap`.
const fn days_before_month(month: u8, leap: bool) -> u64 {
    const BEFORE: [u64; 12] = [0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334];
    let index = month.saturating_sub(1) as usize;
    let days = if index < 12 { BEFORE[index] } else { 0 };
    if leap && month > 2 { days + 1 } else { days }
}

/// Leap years in 1..`year` (proleptic Gregorian).
const fn leap_years_before(year: u64) -> u64 {
    let y = year.saturating_sub(1);
    y / 4 - y / 100 + y / 400
}

/// Days from 1970-01-01 to January 1 of `year` (at least 1970).
const fn days_before_year(year: u64) -> u64 {
    (year - 1970) * 365 + leap_years_before(year) - leap_years_before(1970)
}

/// Decodes one register read. The year is `century × 100 + year` with a century register,
/// else 2000 + year: every platform Origin supports was built after 2000, and a firmware
/// without a century register keeps the RTC within one century.
pub fn decode(r: &Registers) -> Result<DateTime, Field> {
    let binary = r.status_b & MODE_BINARY != 0;
    let value = |raw: u8, field: Field| {
        let v = if binary { Some(raw) } else { from_bcd(raw) };
        v.ok_or(field)
    };
    let second = value(r.seconds, Field::Second).and_then(|v| ok_if(v <= 59, v, Field::Second))?;
    let minute = value(r.minutes, Field::Minute).and_then(|v| ok_if(v <= 59, v, Field::Minute))?;
    let hour = if r.status_b & MODE_24_HOUR != 0 {
        value(r.hours, Field::Hour)?
    } else {
        // 12-hour mode: 12 AM is midnight and 12 PM is noon.
        let h = value(r.hours & 0x7F, Field::Hour)?;
        if !(1..=12).contains(&h) {
            return Err(Field::Hour);
        }
        h % 12 + if r.hours & 0x80 != 0 { 12 } else { 0 }
    };
    if hour > 23 {
        return Err(Field::Hour);
    }
    let yy = value(r.year, Field::Year).and_then(|v| ok_if(v <= 99, v, Field::Year))?;
    let century = match r.century {
        Some(c) => value(c, Field::Century).and_then(|v| ok_if(v <= 99, v, Field::Century))?,
        None => 20,
    };
    let year = u16::from(century) * 100 + u16::from(yy);
    if year < 1970 {
        return Err(Field::Century);
    }
    let month =
        value(r.month, Field::Month).and_then(|v| ok_if((1..=12).contains(&v), v, Field::Month))?;
    let day = value(r.day, Field::Day)?;
    if day == 0 || day > days_in_month(u64::from(year), month) {
        return Err(Field::Day);
    }
    Ok(DateTime {
        year,
        month,
        day,
        hour,
        minute,
        second,
    })
}

fn ok_if(cond: bool, v: u8, field: Field) -> Result<u8, Field> {
    if cond { Ok(v) } else { Err(field) }
}

impl DateTime {
    /// Seconds since 1970-01-01T00:00:00Z (Unix time, no leap seconds), or `None` before
    /// 1970 or for fields out of range.
    pub fn unix_seconds(&self) -> Option<u64> {
        let year = u64::from(self.year);
        let valid = year >= 1970
            && (1..=12).contains(&self.month)
            && self.day >= 1
            && self.day <= days_in_month(year, self.month)
            && self.hour <= 23
            && self.minute <= 59
            && self.second <= 59;
        if !valid {
            return None;
        }
        let days = days_before_year(year)
            + days_before_month(self.month, is_leap(year))
            + u64::from(self.day - 1);
        Some(
            days * 86_400
                + u64::from(self.hour) * 3600
                + u64::from(self.minute) * 60
                + u64::from(self.second),
        )
    }

    /// The UTC date and time `seconds` after 1970-01-01T00:00:00Z, or `None` past the year
    /// 65535.
    pub fn from_unix_seconds(seconds: u64) -> Option<Self> {
        let days = seconds / 86_400;
        let rest = seconds % 86_400;
        // 400 Gregorian years are exactly 146 097 days, so this estimate is within a year
        // of the answer (`days` is below 2^48, so the product cannot overflow).
        let mut year = 1970 + days * 400 / 146_097;
        if year > u64::from(u16::MAX) + 1 {
            return None;
        }
        while days_before_year(year) > days {
            year -= 1;
        }
        while days_before_year(year + 1) <= days {
            year += 1;
        }
        let mut day_of_year = days - days_before_year(year);
        let mut month = 1u8;
        while month < 12 && day_of_year >= u64::from(days_in_month(year, month)) {
            day_of_year -= u64::from(days_in_month(year, month));
            month += 1;
        }
        Some(Self {
            year: u16::try_from(year).ok()?,
            month,
            day: u8::try_from(day_of_year + 1).ok()?,
            hour: u8::try_from(rest / 3600).ok()?,
            minute: u8::try_from(rest % 3600 / 60).ok()?,
            second: u8::try_from(rest % 60).ok()?,
        })
    }
}

/// ISO 8601 in UTC: `2026-11-07T21:42:00Z`.
impl fmt::Display for DateTime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
            self.year, self.month, self.day, self.hour, self.minute, self.second
        )
    }
}

/// How long register A's update-in-progress bit, read clear, guarantees the time
/// registers will not change (the MC146818-compatible PC clock's documented minimum; the
/// M48T86 data sheet states it directly, and Intel's PCHs promise 488 µs). A reading of
/// the seconds register is only known to be valid within this long of the bit being read
/// clear: during an update the time registers may not read correctly.
pub const STABLE_AFTER_CLEAR_MICROS: u64 = 244;

/// The seconds register's value (0–59) in the encoding register B selects, or `None` if
/// it is not a valid seconds value.
pub fn seconds_value(raw: u8, status_b: u8) -> Option<u8> {
    let value = if status_b & MODE_BINARY != 0 {
        raw
    } else {
        from_bcd(raw)?
    };
    (value <= 59).then_some(value)
}

/// One poll of the clock: register A's update-in-progress bit, then the seconds
/// register, read in that order between two readings of a free-running counter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Poll {
    /// The counter before the poll's first register access.
    pub before: u64,
    /// The counter after its last register access.
    pub after: u64,
    /// Register A's update-in-progress bit, as read.
    pub updating: bool,
    /// The seconds register, as read after the bit.
    pub seconds: u8,
}

/// The end of an RTC update, located on the counter as well as the polls could: it
/// happened after `earliest` and no later than `latest`, and `latest` is a reading taken
/// after the new second was already visible.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UpdateEdge {
    /// A counter reading from before the update ended.
    pub earliest: u64,
    /// A counter reading from after the update ended.
    pub latest: u64,
    /// The RTC's seconds count at this edge, from an arbitrary zero: two edges' counts
    /// differ by the seconds between them, however many updates a stall hid.
    pub count: u32,
}

impl UpdateEdge {
    /// How many counter ticks the edge's time is uncertain by.
    pub const fn width(&self) -> u64 {
        self.latest.saturating_sub(self.earliest)
    }
}

/// Finds the updates of the clock's seconds register in a stream of [`Poll`]s.
///
/// An update is seen by the seconds register changing, not by the update-in-progress
/// bit, which is set for as little as 244 µs a second and is easily missed by a
/// processor that is stalled or descheduled across it (a virtual CPU on a loaded host).
/// The bit is still used, to narrow the edge: a poll that finds it set proves the update
/// had not ended when that poll began, and so does a valid reading of the old second.
/// On a real part the bit is up for the whole update, so the edge is located to a poll
/// or two.
///
/// Only documented behaviour is relied on: a clear bit guarantees the registers stay
/// valid for [`STABLE_AFTER_CLEAR_MICROS`], so a reading taken with the bit set, or on a
/// poll too slow to have stayed within that guarantee, is not used for anything.
#[derive(Clone, Copy, Debug)]
pub struct UpdateWatcher {
    /// Register B, which gives the seconds register's encoding.
    status_b: u8,
    /// Counter ticks in the time a clear bit guarantees.
    max_poll: u64,
    /// The start of the last poll showing the next update had not yet ended.
    not_ended: Option<u64>,
    /// The seconds value at the last valid reading.
    seconds: Option<u8>,
    /// Seconds counted since the first valid reading.
    count: u32,
}

impl UpdateWatcher {
    /// A watcher for a clock whose register B is `status_b`, read against a counter
    /// running at about `hz`.
    pub fn new(status_b: u8, hz: u64) -> Self {
        let max_poll = u128::from(hz) * u128::from(STABLE_AFTER_CLEAR_MICROS) / 1_000_000;
        Self {
            status_b,
            max_poll: u64::try_from(max_poll).unwrap_or(u64::MAX),
            not_ended: None,
            seconds: None,
            count: 0,
        }
    }

    /// Takes one poll; returns the update it shows has ended, if any. A stall of more
    /// than a second shows as one edge that counts every second it hid.
    pub fn observe(&mut self, poll: Poll) -> Option<UpdateEdge> {
        if poll.updating {
            // An update is under way or about to start, so it had not ended when this
            // poll began; a seconds reading taken now may not be valid.
            self.not_ended = Some(poll.before);
            return None;
        }
        if poll.after.saturating_sub(poll.before) > self.max_poll {
            // The poll took too long for the clear bit to vouch for its reading.
            return None;
        }
        let value = seconds_value(poll.seconds, self.status_b)?;
        // The seconds were read at or after `poll.before`, so the next update has not
        // ended before then.
        let last = self.seconds.replace(value);
        let not_ended = self.not_ended.replace(poll.before);
        let (last, earliest) = (last?, not_ended?);
        if value == last {
            return None;
        }
        // The change itself is proof the update ended by `poll.after`.
        let steps = u32::from((value + 60 - last) % 60);
        self.count = self.count.saturating_add(steps);
        Some(UpdateEdge {
            earliest,
            latest: poll.after,
            count: self.count,
        })
    }
}

/// What an RTC second measured on a counter says about the counter's expected rate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SecondVerdict {
    /// Every value the measurement allows is within the tolerance.
    Confirmed,
    /// Every value the measurement allows is outside it.
    Refuted,
    /// The measurement cannot tell: it allows values on both sides.
    Undecided,
}

/// An RTC second measured on a counter, between two update edges some seconds apart.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SecondMeasurement {
    /// The fewest counter ticks one RTC second can have lasted (rounded down).
    pub min_ticks: u64,
    /// The most it can have lasted (rounded up).
    pub max_ticks: u64,
    /// The RTC seconds between the two edges.
    pub seconds: u32,
    /// What it says about the expected rate.
    pub verdict: SecondVerdict,
}

impl SecondMeasurement {
    /// The width of what the edges allow, in ticks per RTC second.
    pub const fn width(&self) -> u64 {
        self.max_ticks.saturating_sub(self.min_ticks)
    }

    /// The best single estimate of one RTC second: the middle of what the edges allow.
    pub const fn ticks(&self) -> u64 {
        self.min_ticks + self.width() / 2
    }

    /// How far, at most, the true second is from [`SecondMeasurement::ticks`].
    pub const fn error_ticks(&self) -> u64 {
        self.width().div_ceil(2)
    }
}

/// Edges a [`SecondMeter`] keeps: enough for every edge of its budget (at most four
/// edges fall in 3.2 s), and a measurement can use any two of them.
const METER_EDGES: usize = 4;

/// Decides whether the RTC's second agrees with a counter's expected rate, from
/// [`UpdateEdge`]s (CLAUDE.md §68: timer calibration).
///
/// Every edge is known only to within its bracket, so every measurement is an interval
/// that is guaranteed to contain the true second. A measurement decides only if the whole
/// interval lies inside the tolerance ([`SecondVerdict::Confirmed`]) or outside it
/// ([`SecondVerdict::Refuted`]). An undecided one gives way to the next edge: a stall
/// costs a measurement a second, never a wrong answer, and a longer baseline shrinks the
/// uncertainty by as much.
#[derive(Clone, Copy, Debug)]
pub struct SecondMeter {
    /// The ticks one RTC second should last on the counter.
    expected: u64,
    /// The tolerance, in parts per thousand of `expected`.
    tolerance: u16,
    /// Up to [`METER_EDGES`] edges, sharpest kept when full.
    edges: [Option<UpdateEdge>; METER_EDGES],
    /// The tightest measurement so far.
    tightest: Option<SecondMeasurement>,
}

impl SecondMeter {
    /// A meter for a counter that should run at `expected` ticks per RTC second, within
    /// `tolerance` parts per thousand.
    pub const fn new(expected: u64, tolerance: u16) -> Self {
        Self {
            expected,
            tolerance,
            edges: [None; METER_EDGES],
            tightest: None,
        }
    }

    /// Takes the next edge. Returns the measurement that decides the matter (the tightest
    /// decisive one that this edge completes), if one does.
    pub fn add(&mut self, edge: UpdateEdge) -> Option<SecondMeasurement> {
        let mut decided: Option<SecondMeasurement> = None;
        let earlier = self.edges;
        for first in earlier.iter().flatten() {
            let Some(measured) = self.measure(first, &edge) else {
                continue;
            };
            if self
                .tightest
                .is_none_or(|tightest| measured.width() < tightest.width())
            {
                self.tightest = Some(measured);
            }
            if measured.verdict != SecondVerdict::Undecided
                && decided.is_none_or(|best| measured.width() < best.width())
            {
                decided = Some(measured);
            }
        }
        self.keep(edge);
        decided
    }

    /// The tightest measurement so far, decisive or not.
    pub const fn tightest(&self) -> Option<SecondMeasurement> {
        self.tightest
    }

    /// The measurement between two edges, if they are seconds apart.
    fn measure(&self, first: &UpdateEdge, last: &UpdateEdge) -> Option<SecondMeasurement> {
        let seconds = last.count.checked_sub(first.count).filter(|&n| n > 0)?;
        let n = u64::from(seconds);
        let min_ticks = last.earliest.saturating_sub(first.latest) / n;
        let max_ticks = last.latest.saturating_sub(first.earliest).div_ceil(n);
        let (min_ticks, max_ticks) = (min_ticks.min(max_ticks), max_ticks.max(min_ticks));
        Some(SecondMeasurement {
            min_ticks,
            max_ticks,
            seconds,
            verdict: self.verdict(min_ticks, max_ticks),
        })
    }

    /// Compares a measurement's whole interval with the tolerance.
    fn verdict(&self, min_ticks: u64, max_ticks: u64) -> SecondVerdict {
        const PARTS: u128 = 1000;
        let tolerance = u128::from(self.tolerance);
        let expected = u128::from(self.expected);
        let low = expected * PARTS.saturating_sub(tolerance);
        let high = expected * (PARTS + tolerance);
        let (min_ticks, max_ticks) = (u128::from(min_ticks) * PARTS, u128::from(max_ticks) * PARTS);
        if min_ticks >= low && max_ticks <= high {
            SecondVerdict::Confirmed
        } else if max_ticks < low || min_ticks > high {
            SecondVerdict::Refuted
        } else {
            SecondVerdict::Undecided
        }
    }

    /// Stores `edge`, replacing the widest kept edge if the slots are full and `edge` is
    /// sharper: a sharp edge is worth more to the measurements still to come.
    fn keep(&mut self, edge: UpdateEdge) {
        if let Some(slot) = self.edges.iter_mut().find(|slot| slot.is_none()) {
            *slot = Some(edge);
        } else if let Some(widest) = self.edges.iter_mut().flatten().max_by_key(|e| e.width())
            && edge.width() < widest.width()
        {
            *widest = edge;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dt(year: u16, month: u8, day: u8, hour: u8, minute: u8, second: u8) -> DateTime {
        DateTime {
            year,
            month,
            day,
            hour,
            minute,
            second,
        }
    }

    /// QEMU's default RTC mode: BCD, 24-hour.
    fn bcd(t: DateTime, century: bool) -> Registers {
        let b = |v: u16| {
            let v = (v % 100) as u8;
            ((v / 10) << 4) | (v % 10)
        };
        Registers {
            seconds: b(u16::from(t.second)),
            minutes: b(u16::from(t.minute)),
            hours: b(u16::from(t.hour)),
            day: b(u16::from(t.day)),
            month: b(u16::from(t.month)),
            year: b(t.year),
            century: century.then(|| b(t.year / 100)),
            status_b: MODE_24_HOUR,
        }
    }

    /// (year, month, day, hour, minute, second) and its Unix time.
    type Known = ((u16, u8, u8, u8, u8, u8), u64);

    /// Reference values from an independent implementation (Python's `datetime`, UTC).
    const KNOWN: [Known; 9] = [
        ((1970, 1, 1, 0, 0, 0), 0),
        ((2000, 1, 1, 0, 0, 0), 946_684_800),
        ((2000, 2, 29, 0, 0, 0), 951_782_400),
        ((2026, 11, 7, 0, 0, 0), 1_794_009_600),
        ((2030, 6, 15, 12, 0, 0), 1_907_755_200),
        ((2038, 1, 19, 3, 14, 7), 2_147_483_647),
        ((2099, 12, 31, 23, 59, 59), 4_102_444_799),
        ((2100, 3, 1, 0, 0, 0), 4_107_542_400),
        ((2400, 2, 29, 12, 34, 56), 13_574_608_496),
    ];

    #[test]
    fn unix_time_matches_the_reference() {
        for ((y, mo, d, h, mi, s), unix) in KNOWN {
            let t = dt(y, mo, d, h, mi, s);
            assert_eq!(t.unix_seconds(), Some(unix), "{t}");
            assert_eq!(DateTime::from_unix_seconds(unix), Some(t), "{unix}");
        }
    }

    #[test]
    fn every_day_from_1970_to_2500_round_trips() {
        let mut expected_days = 0u64;
        for year in 1970u16..2500 {
            for month in 1..=12u8 {
                for day in 1..=days_in_month(u64::from(year), month) {
                    let t = dt(year, month, day, 23, 59, 59);
                    let unix = t.unix_seconds().unwrap();
                    assert_eq!(unix, expected_days * 86_400 + 86_399, "{t}");
                    assert_eq!(DateTime::from_unix_seconds(unix), Some(t));
                    let midnight = dt(year, month, day, 0, 0, 0);
                    assert_eq!(midnight.unix_seconds(), Some(expected_days * 86_400));
                    assert_eq!(
                        DateTime::from_unix_seconds(expected_days * 86_400),
                        Some(midnight)
                    );
                    expected_days += 1;
                }
            }
        }
        // 530 years of 365 days plus the leap days among them (2000 and 2400 are leap
        // years; 2100, 2200 and 2300 are not).
        assert_eq!(expected_days, 530 * 365 + 129);
    }

    #[test]
    fn leap_years() {
        for (year, leap) in [
            (1900, false),
            (2000, true),
            (2024, true),
            (2026, false),
            (2100, false),
            (2400, true),
        ] {
            assert_eq!(is_leap(year), leap, "{year}");
        }
        assert_eq!(days_in_month(2100, 2), 28);
        assert_eq!(days_in_month(2000, 2), 29);
        assert_eq!(days_in_month(2026, 13), 0);
    }

    #[test]
    fn bcd_24_hour_with_and_without_a_century_register() {
        let t = dt(2030, 6, 15, 12, 0, 5);
        assert_eq!(decode(&bcd(t, true)), Ok(t));
        assert_eq!(
            decode(&bcd(t, false)),
            Ok(t),
            "20xx without a century register"
        );
        // The century register changes the year.
        let mut r = bcd(t, true);
        r.century = Some(0x21);
        assert_eq!(decode(&r).map(|d| d.year), Ok(2130));
    }

    #[test]
    fn binary_mode() {
        let r = Registers {
            seconds: 59,
            minutes: 30,
            hours: 23,
            day: 31,
            month: 12,
            year: 26,
            century: Some(20),
            status_b: MODE_24_HOUR | MODE_BINARY,
        };
        assert_eq!(decode(&r), Ok(dt(2026, 12, 31, 23, 30, 59)));
    }

    #[test]
    fn twelve_hour_mode() {
        let at = |hours: u8| {
            let mut r = bcd(dt(2026, 11, 7, 0, 0, 0), true);
            r.status_b = 0; // BCD, 12-hour
            r.hours = hours;
            decode(&r).map(|d| d.hour)
        };
        assert_eq!(at(0x12), Ok(0), "12 AM is midnight");
        assert_eq!(at(0x01), Ok(1));
        assert_eq!(at(0x11), Ok(11));
        assert_eq!(at(0x92), Ok(12), "12 PM is noon");
        assert_eq!(at(0x81), Ok(13));
        assert_eq!(at(0x91), Ok(23));
        assert_eq!(at(0x00), Err(Field::Hour));
        assert_eq!(at(0x13), Err(Field::Hour));
        assert_eq!(at(0x80), Err(Field::Hour));
    }

    #[test]
    fn invalid_registers_are_refused() {
        let good = bcd(dt(2026, 2, 28, 10, 20, 30), true);
        assert!(decode(&good).is_ok());
        type Edit = fn(&mut Registers);
        let cases: [(Edit, Field); 12] = [
            (|r| r.seconds = 0x60, Field::Second),
            (|r| r.seconds = 0x0A, Field::Second), // not a BCD digit
            (|r| r.minutes = 0x7F, Field::Minute),
            (|r| r.hours = 0x24, Field::Hour),
            (|r| r.hours = 0xA0, Field::Hour),
            (|r| r.day = 0x00, Field::Day),
            (|r| r.day = 0x29, Field::Day), // 2026 is not a leap year
            (|r| r.month = 0x00, Field::Month),
            (|r| r.month = 0x13, Field::Month),
            (|r| r.year = 0x9A, Field::Year),
            (|r| r.century = Some(0x19), Field::Century), // 1926: before Unix time
            (|r| r.century = Some(0xFF), Field::Century),
        ];
        for (edit, field) in cases {
            let mut r = good;
            edit(&mut r);
            assert_eq!(decode(&r), Err(field), "{r:?}");
        }
        // Binary mode does not accept values BCD would.
        let mut r = good;
        r.status_b = MODE_24_HOUR | MODE_BINARY;
        r.seconds = 60;
        assert_eq!(decode(&r), Err(Field::Second));
    }

    /// Encodes `t` as the RTC would in `mode` (register B's binary and 24-hour bits).
    fn encode(t: DateTime, mode: u8, century: bool) -> Registers {
        let enc = |v: u8| {
            if mode & MODE_BINARY != 0 {
                v
            } else {
                ((v / 10) << 4) | (v % 10)
            }
        };
        let hours = if mode & MODE_24_HOUR != 0 {
            enc(t.hour)
        } else {
            let (h, pm) = match t.hour {
                0 => (12, false),
                1..=11 => (t.hour, false),
                12 => (12, true),
                h => (h - 12, true),
            };
            enc(h) | if pm { 0x80 } else { 0 }
        };
        Registers {
            seconds: enc(t.second),
            minutes: enc(t.minute),
            hours,
            day: enc(t.day),
            month: enc(t.month),
            year: enc((t.year % 100) as u8),
            century: century.then(|| enc((t.year / 100) as u8)),
            status_b: mode,
        }
    }

    /// Every real date round-trips through each register encoding, and one corrupted
    /// register never decodes to anything but a real date (CLAUDE.md §72).
    #[test]
    fn encodings_round_trip_and_corrupted_registers_decode_only_to_real_dates() {
        let mut rng = crate::testing::Rng::new(0x5254_4331);
        let (mut corrupted_accepted, mut corrupted_refused) = (0, 0);
        for case in 0..crate::testing::cases(100_000) {
            let unix = rng.next() % 4_102_444_800; // 1970 to 2099
            let mut t = DateTime::from_unix_seconds(unix).unwrap();
            if t.year < 2000 {
                t.year += 100; // without a century register the RTC means 20xx
            }
            let mode = [0, MODE_24_HOUR, MODE_BINARY, MODE_24_HOUR | MODE_BINARY][rng.below(4)];
            let century = rng.below(2) == 0;
            let mut r = encode(t, mode, century);
            if t.day <= days_in_month(u64::from(t.year), t.month) {
                assert_eq!(decode(&r), Ok(t), "case {case}: {r:?}");
            }
            // One register replaced with a random value.
            let v = rng.byte();
            match rng.below(7) {
                0 => r.seconds = v,
                1 => r.minutes = v,
                2 => r.hours = v,
                3 => r.day = v,
                4 => r.month = v,
                5 => r.year = v,
                _ => r.century = Some(v),
            }
            match decode(&r) {
                Ok(d) => {
                    corrupted_accepted += 1;
                    assert!(
                        d.second <= 59 && d.minute <= 59 && d.hour <= 23,
                        "case {case}: {d:?}"
                    );
                    assert!((1..=12).contains(&d.month), "case {case}: {d:?}");
                    assert!(
                        d.day >= 1 && d.day <= days_in_month(u64::from(d.year), d.month),
                        "case {case}: {d:?}"
                    );
                    let back = d.unix_seconds().unwrap();
                    assert_eq!(DateTime::from_unix_seconds(back), Some(d), "case {case}");
                }
                Err(_) => corrupted_refused += 1,
            }
        }
        // Both outcomes were reached many times.
        assert!(
            corrupted_accepted > 10_000 && corrupted_refused > 10_000,
            "{corrupted_accepted} accepted, {corrupted_refused} refused"
        );
    }

    /// Every register random at once (the other soak corrupts one at a time): whatever
    /// decodes is a real date, re-encodes to the registers it came from, and survives the
    /// trip through Unix time (CLAUDE.md §72).
    #[test]
    fn fully_random_registers_decode_only_to_real_dates() {
        use crate::testing::Rng;
        let mut rng = Rng::new(0x5254_4332);
        let mut accepted = 0u32;
        // Most random registers are not dates, so aim each field near its valid range.
        let field = |rng: &mut Rng, max: u8, bcd: bool| {
            let v = rng.below(usize::from(max) + 2) as u8;
            if rng.below(16) == 0 {
                rng.byte()
            } else if bcd {
                ((v / 10) << 4) | (v % 10)
            } else {
                v
            }
        };
        for case in 0..crate::testing::cases(200_000) {
            let status_b = rng.byte();
            let bcd = status_b & MODE_BINARY == 0;
            let r = Registers {
                seconds: field(&mut rng, 59, bcd),
                minutes: field(&mut rng, 59, bcd),
                hours: field(&mut rng, 23, bcd) | if rng.below(4) == 0 { 0x80 } else { 0 },
                day: field(&mut rng, 31, bcd),
                month: field(&mut rng, 12, bcd),
                year: field(&mut rng, 99, bcd),
                century: if rng.below(2) == 0 {
                    Some(field(&mut rng, 99, bcd))
                } else {
                    None
                },
                status_b,
            };
            let Ok(d) = decode(&r) else { continue };
            accepted += 1;
            assert!(
                d.second <= 59 && d.minute <= 59 && d.hour <= 23,
                "case {case}: {d:?}"
            );
            assert!((1..=12).contains(&d.month), "case {case}: {d:?}");
            let len = days_in_month(u64::from(d.year), d.month);
            assert!(d.day >= 1 && d.day <= len, "case {case}: {d:?}");
            assert!(d.year >= 1970, "case {case}: {d:?}");
            let again = encode(d, status_b, r.century.is_some());
            assert_eq!(
                (
                    again.seconds,
                    again.minutes,
                    again.hours,
                    again.day,
                    again.month,
                    again.year
                ),
                (r.seconds, r.minutes, r.hours, r.day, r.month, r.year),
                "case {case}: {r:?} -> {d:?}"
            );
            if r.century.is_some() {
                assert_eq!(again.century, r.century, "case {case}");
            }
            let back = d.unix_seconds().unwrap();
            assert_eq!(DateTime::from_unix_seconds(back), Some(d), "case {case}");
        }
        assert!(accepted > 3000, "{accepted}");
    }

    /// The calendar arithmetic over any 64-bit input: Unix time and back agree, and fields
    /// that cannot be a date have no Unix time.
    #[test]
    fn calendar_arithmetic_is_total_over_any_input() {
        use crate::testing::Rng;
        let mut rng = Rng::new(0x4341_4C31);
        for case in 0..crate::testing::cases(20_000) {
            let seconds = rng.edge_u64();
            if let Some(d) = DateTime::from_unix_seconds(seconds) {
                assert_eq!(d.unix_seconds(), Some(seconds), "case {case}: {seconds}");
                let _ = format!("{d}");
            }
            let t = dt(
                rng.next() as u16,
                rng.byte(),
                rng.byte(),
                rng.byte(),
                rng.byte(),
                rng.byte(),
            );
            if t.unix_seconds().is_some() {
                let ok = t.year >= 1970
                    && (1..=12).contains(&t.month)
                    && t.day >= 1
                    && t.day <= days_in_month(u64::from(t.year), t.month)
                    && t.hour <= 23
                    && t.minute <= 59
                    && t.second <= 59;
                assert!(ok, "case {case}: {t:?}");
            }
            let year = rng.edge_u64();
            let month = rng.byte();
            let n = days_in_month(year, month);
            assert!(
                (28..=31).contains(&n) == (1..=12).contains(&month) && (n <= 31),
                "case {case}"
            );
        }
    }

    #[test]
    fn invalid_dates_have_no_unix_time() {
        assert_eq!(dt(1969, 12, 31, 23, 59, 59).unix_seconds(), None);
        assert_eq!(dt(2026, 2, 29, 0, 0, 0).unix_seconds(), None);
        assert_eq!(dt(2026, 4, 31, 0, 0, 0).unix_seconds(), None);
        assert_eq!(dt(2026, 1, 1, 24, 0, 0).unix_seconds(), None);
        assert_eq!(dt(2026, 0, 1, 0, 0, 0).unix_seconds(), None);
    }

    #[test]
    fn display_is_iso_8601_utc() {
        assert_eq!(
            format!("{}", dt(2026, 11, 7, 9, 5, 0)),
            "2026-11-07T09:05:00Z"
        );
        assert_eq!(
            DateTime::from_unix_seconds(u64::MAX).map(|_| ()),
            None,
            "beyond 65535 has no representation"
        );
    }

    #[test]
    fn seconds_value_decodes_both_encodings_and_refuses_nonsense() {
        assert_eq!(seconds_value(0x59, MODE_24_HOUR), Some(59));
        assert_eq!(seconds_value(0x00, 0), Some(0));
        assert_eq!(seconds_value(59, MODE_BINARY), Some(59));
        assert_eq!(seconds_value(0x60, MODE_24_HOUR), None, "sixty seconds");
        assert_eq!(seconds_value(0x7A, MODE_24_HOUR), None, "a nibble above 9");
        assert_eq!(seconds_value(60, MODE_BINARY), None);
        assert_eq!(seconds_value(0xFF, MODE_24_HOUR), None, "a floating bus");
    }

    const HZ: u64 = 1_000_000_000; // one tick is one nanosecond
    const MS: u64 = HZ / 1000;
    const US: u64 = HZ / 1_000_000;

    /// A poll of the clock from `before` that read the bit and the seconds in 3 µs.
    fn poll(before: u64, updating: bool, seconds: u8) -> Poll {
        Poll {
            before,
            after: before + 3 * US,
            updating,
            seconds,
        }
    }

    #[test]
    fn an_edge_lies_between_the_last_poll_of_the_old_second_and_the_first_of_the_new() {
        let mut w = UpdateWatcher::new(MODE_24_HOUR, HZ);
        assert_eq!(
            w.observe(poll(100 * MS, false, 0x05)),
            None,
            "first reading"
        );
        assert_eq!(w.observe(poll(200 * MS, false, 0x05)), None, "same second");
        // The bit proves the update has not ended by this poll's start.
        assert_eq!(w.observe(poll(300 * MS, true, 0x05)), None);
        assert_eq!(
            w.observe(poll(400 * MS, false, 0x06)),
            Some(UpdateEdge {
                earliest: 300 * MS,
                latest: 400 * MS + 3 * US,
                count: 1
            })
        );
    }

    /// The failure this code exists to prevent: a poll that never catches the 244 µs
    /// update-in-progress bit still finds the update, and only loses precision.
    #[test]
    fn a_missed_update_in_progress_pulse_still_gives_the_edge() {
        let mut w = UpdateWatcher::new(0, HZ);
        assert_eq!(w.observe(poll(10 * MS, false, 0x30)), None);
        // No poll saw the bit: the processor was stalled through the last 244 µs.
        let edge = w.observe(poll(1200 * MS, false, 0x31)).unwrap();
        assert_eq!(edge.earliest, 10 * MS);
        assert_eq!(edge.latest, 1200 * MS + 3 * US);
        assert_eq!(edge.count, 1);
    }

    #[test]
    fn readings_the_bit_cannot_vouch_for_are_ignored() {
        let mut w = UpdateWatcher::new(MODE_24_HOUR, HZ);
        assert_eq!(w.observe(poll(0, false, 0x10)), None);
        // A set bit: the value read is meaningless (even an impossible one).
        assert_eq!(w.observe(poll(MS, true, 0xFF)), None);
        // A poll too long for the clear bit to vouch for its reading.
        let slow = Poll {
            before: 2 * MS,
            after: 2 * MS + 245 * US,
            updating: false,
            seconds: 0x11,
        };
        assert_eq!(w.observe(slow), None);
        // An invalid value in a fast poll is ignored too.
        assert_eq!(w.observe(poll(3 * MS, false, 0xF0)), None);
        // The last good reading is still the old second; this edge is the real change.
        let edge = w.observe(poll(10 * MS, false, 0x11)).unwrap();
        assert_eq!(edge.count, 1);
        assert_eq!(edge.latest, 10 * MS + 3 * US);
        // A poll just within the guarantee is believed.
        let just_fast = Poll {
            before: 20 * MS,
            after: 20 * MS + 244 * US,
            updating: false,
            seconds: 0x12,
        };
        assert_eq!(w.observe(just_fast).map(|e| e.count), Some(2));
    }

    #[test]
    fn stalls_are_counted_through_the_minute_wrap_in_both_encodings() {
        for (mode, a, b, c) in [
            (MODE_24_HOUR, 0x58u8, 0x59u8, 0x00u8),
            (MODE_24_HOUR | MODE_BINARY, 58, 59, 0),
        ] {
            let mut w = UpdateWatcher::new(mode, HZ);
            assert_eq!(w.observe(poll(0, false, a)), None);
            assert_eq!(w.observe(poll(HZ, false, b)).map(|e| e.count), Some(1));
            assert_eq!(w.observe(poll(2 * HZ, false, c)).map(|e| e.count), Some(2));
        }
        // A stall of three seconds across the wrap is one edge worth three seconds.
        let mut w = UpdateWatcher::new(MODE_24_HOUR, HZ);
        assert_eq!(w.observe(poll(0, false, 0x59)), None);
        assert_eq!(
            w.observe(poll(3 * HZ, false, 0x02)).map(|e| e.count),
            Some(3)
        );
    }

    /// An edge whose update ended somewhere in `[earliest, latest]` (the edge's bracket).
    fn edge(earliest: u64, latest: u64, count: u32) -> UpdateEdge {
        UpdateEdge {
            earliest,
            latest,
            count,
        }
    }

    #[test]
    fn the_meter_decides_only_on_a_whole_interval_inside_or_outside_the_tolerance() {
        // A narrow pair one second apart, dead on: confirmed.
        let mut m = SecondMeter::new(HZ, 10);
        assert_eq!(m.add(edge(0, 10 * US, 0)), None, "one edge decides nothing");
        let got = m.add(edge(HZ, HZ + 10 * US, 1)).unwrap();
        assert_eq!(got.verdict, SecondVerdict::Confirmed);
        assert_eq!(got.seconds, 1);
        assert_eq!(got.min_ticks, HZ - 10 * US);
        assert_eq!(got.max_ticks, HZ + 10 * US);
        assert_eq!(got.ticks(), HZ);
        assert_eq!(got.error_ticks(), 10 * US);

        // Three per cent slow and fast: refuted, whole interval outside.
        for second in [HZ + 30 * MS, HZ - 30 * MS] {
            let mut m = SecondMeter::new(HZ, 10);
            assert_eq!(m.add(edge(0, 10 * US, 0)), None);
            let got = m.add(edge(second, second + 10 * US, 1)).unwrap();
            assert_eq!(got.verdict, SecondVerdict::Refuted, "{second}");
        }

        // An interval straddling the 1 % limit decides nothing.
        let mut m = SecondMeter::new(HZ, 10);
        assert_eq!(m.add(edge(0, 5 * MS, 0)), None);
        assert_eq!(m.add(edge(HZ + 8 * MS, HZ + 12 * MS, 1)), None);
        let tightest = m.tightest().unwrap();
        assert_eq!(tightest.verdict, SecondVerdict::Undecided);
        assert_eq!(tightest.seconds, 1);

        // A wide pair inside the tolerance is still confirmed: the whole interval is in.
        let mut m = SecondMeter::new(HZ, 10);
        assert_eq!(m.add(edge(0, 4 * MS, 0)), None);
        assert_eq!(
            m.add(edge(HZ - 2 * MS, HZ + 2 * MS, 1)).map(|d| d.verdict),
            Some(SecondVerdict::Confirmed)
        );
    }

    /// A stall spoils a pair; the next edge decides, over the longer baseline, and the
    /// result is divided by the seconds it spans.
    #[test]
    fn an_undecided_pair_gives_way_to_a_longer_baseline() {
        // Edge one is sharp, edge two 25 ms vague: 1 % of a second cannot tell.
        let mut m = SecondMeter::new(HZ, 10);
        assert_eq!(m.add(edge(0, US, 10)), None);
        assert_eq!(m.add(edge(HZ - 12 * MS, HZ + 13 * MS, 11)), None);
        assert_eq!(m.tightest().unwrap().verdict, SecondVerdict::Undecided);
        // Two seconds from the first edge, sharp: confirmed, and the pair chosen is the
        // tightest decisive one.
        let got = m.add(edge(2 * HZ, 2 * HZ + US, 12)).unwrap();
        assert_eq!(got.verdict, SecondVerdict::Confirmed);
        assert_eq!(got.seconds, 2);
        assert_eq!(got.ticks(), HZ);

        // A measurement over two seconds of a wrong clock is refuted by the same rule.
        let mut m = SecondMeter::new(HZ, 10);
        assert_eq!(m.add(edge(0, US, 0)), None);
        let got = m.add(edge(2 * HZ + 60 * MS, 2 * HZ + 61 * MS, 2)).unwrap();
        assert_eq!(got.verdict, SecondVerdict::Refuted);
        assert_eq!(got.ticks(), 1_030_249_750);

        // A stall that hid a second counts: 3 RTC seconds, not 2.
        let mut m = SecondMeter::new(HZ, 10);
        assert_eq!(m.add(edge(0, US, 57)), None);
        let got = m.add(edge(3 * HZ, 3 * HZ + US, 60)).unwrap();
        assert_eq!((got.seconds, got.verdict), (3, SecondVerdict::Confirmed));

        // Wider edges are forgotten first once the meter is full, never a sharp one.
        let mut m = SecondMeter::new(HZ, 10);
        for (i, width) in [40 * MS, 30 * MS, 20 * MS, 10 * MS].into_iter().enumerate() {
            let at = (i as u64 + 1) * HZ;
            assert_eq!(m.add(edge(at, at + width, i as u32)), None, "{i}");
        }
        // The fifth edge is sharp and replaces the widest.
        let _ = m.add(edge(5 * HZ, 5 * HZ + US, 4));
        let kept: u64 = m
            .edges
            .iter()
            .flatten()
            .map(UpdateEdge::width)
            .max()
            .unwrap();
        assert_eq!(kept, 30 * MS, "the 40 ms edge went");
    }

    /// One model clock for the proof's tests, in counter ticks at [`HZ`]. Its update
    /// number `n` ends at `first + n × period`.
    #[derive(Clone, Copy)]
    struct ModelClock {
        first: u64,
        period: u64,
        /// The bit is set for this long before each update ends.
        uip: u64,
        /// The seconds read as garbage for this long before each update ends: inside the
        /// bit's window, and at least 244 µs after the bit first shows (the documented
        /// guarantee).
        mid_update: u64,
        /// The seconds before the first update, in decimal.
        start: u8,
        /// Register B: the encoding.
        mode: u8,
    }

    impl ModelClock {
        /// How many updates have ended by `t`.
        fn ended(&self, t: u64) -> u64 {
            if t < self.first {
                0
            } else {
                (t - self.first) / self.period + 1
            }
        }

        /// The end of update `n`.
        fn end_of(&self, n: u64) -> u64 {
            self.first + n * self.period
        }

        /// The end of the first update still to come at `t`.
        fn next_end(&self, t: u64) -> u64 {
            self.end_of(self.ended(t))
        }

        fn bit_set(&self, t: u64) -> bool {
            t.saturating_add(self.uip) >= self.next_end(t)
        }

        fn seconds_register(&self, t: u64, rng: &mut Rng) -> u8 {
            if t.saturating_add(self.mid_update) >= self.next_end(t) {
                return rng.byte(); // a read during the update is anything at all
            }
            let value = ((u64::from(self.start) + self.ended(t)) % 60) as u8;
            if self.mode & MODE_BINARY != 0 {
                value
            } else {
                ((value / 10) << 4) | (value % 10)
            }
        }
    }

    use crate::testing::Rng;

    /// A clock with QEMU 11.1's timing: the bit up for the last 244 µs of the second,
    /// and the seconds register never mid-update.
    fn qemu_clock(period: u64, rng: &mut Rng) -> ModelClock {
        ModelClock {
            first: rng.next() % period + 1,
            period,
            uip: 244 * US,
            mid_update: 0,
            start: rng.below(60) as u8,
            mode: MODE_24_HOUR,
        }
    }

    /// A clock with a real part's timing: the bit up 244 µs before an update of 1984 µs.
    fn hardware_clock(period: u64, rng: &mut Rng) -> ModelClock {
        ModelClock {
            first: rng.next() % period + 1,
            period,
            uip: (244 + 1984) * US,
            mid_update: 1984 * US,
            start: rng.below(60) as u8,
            mode: if rng.below(2) == 0 {
                MODE_24_HOUR
            } else {
                MODE_24_HOUR | MODE_BINARY
            },
        }
    }

    /// How the polling processor is stalled: in `one_in` polls (0: none) a stall of up to
    /// `max` ticks anywhere in the poll; and for one update in `at_edge_one_in` (0: none)
    /// a stall of up to `at_edge_max` across the update's end, the case a descheduled
    /// virtual CPU produces.
    #[derive(Clone, Copy)]
    struct Stalls {
        one_in: usize,
        max: u64,
        at_edge_one_in: usize,
        at_edge_max: u64,
    }

    const NO_STALLS: Stalls = Stalls {
        one_in: 0,
        max: 0,
        at_edge_one_in: 0,
        at_edge_max: 0,
    };
    /// What the kernel's RTC budget is: 3.2 s of polling.
    const BUDGET: u64 = 3 * HZ + HZ / 5;

    /// What a run of the proof against a model clock showed.
    #[derive(Clone, Copy, Debug, Default)]
    struct Proof {
        decided: Option<SecondMeasurement>,
        tightest: Option<SecondMeasurement>,
        /// Edges seen before deciding.
        edges: usize,
        /// Simulated time from the first poll to the decision.
        spent: u64,
    }

    /// The kernel's proof (`time::measure_rtc`) against `clock`, for a counter expected
    /// to run at `expected` Hz; `stalls` freeze the polling processor. Checks on the way
    /// that every edge's bracket really contains the update it reports.
    fn prove(clock: &ModelClock, expected: u64, stalls: Stalls, rng: &mut Rng) -> Proof {
        let mut watcher = UpdateWatcher::new(clock.mode, expected);
        let mut meter = SecondMeter::new(expected, 10);
        let mut proof = Proof::default();
        let start = rng.next() % (clock.period.min(2 * HZ)) + 1;
        let mut t = start;
        let mut last_stalled = None;
        while t.saturating_sub(start) <= BUDGET {
            let update = clock.ended(t);
            let to_end = clock.next_end(t).saturating_sub(t);
            if to_end < 300 * US
                && last_stalled != Some(update)
                && stalls.at_edge_one_in != 0
                && rng.below(stalls.at_edge_one_in) == 0
            {
                // Descheduled across this update's end.
                t += to_end + rng.next() % stalls.at_edge_max.saturating_sub(to_end).max(1);
                last_stalled = Some(update);
            }
            let stall = |rng: &mut Rng| {
                if stalls.one_in != 0 && rng.below(stalls.one_in) == 0 {
                    rng.next() % (stalls.max + 1)
                } else {
                    0
                }
            };
            let before = t + stall(rng);
            let bit_read = before + US + stall(rng);
            let updating = clock.bit_set(bit_read);
            let seconds_read = bit_read + US + stall(rng);
            let seconds = clock.seconds_register(seconds_read, rng);
            let after = seconds_read + US + stall(rng);
            let poll = Poll {
                before,
                after,
                updating,
                seconds,
            };
            // Far from an update, poll sparsely: the proof does not care and the test is
            // much faster. Within 20 ms of an update on either side, poll every few
            // hundred µs.
            let near = clock.next_end(after) - after <= 20 * MS
                || clock
                    .ended(after)
                    .checked_sub(1)
                    .is_some_and(|n| after - clock.end_of(n) <= 20 * MS);
            t = after
                + if near {
                    rng.next() % (300 * US)
                } else {
                    10 * MS
                };
            let Some(edge) = watcher.observe(poll) else {
                continue;
            };
            proof.edges += 1;
            let reported = clock.ended(seconds_read) - 1; // the update the new value shows
            let end = clock.end_of(reported);
            assert!(
                edge.earliest < end && end <= edge.latest,
                "edge {edge:?} does not contain update {reported}, which ended at {end}"
            );
            if let Some(decided) = meter.add(edge) {
                proof.decided = Some(decided);
                proof.spent = seconds_read - start;
                break;
            }
        }
        proof.tightest = meter.tightest();
        proof
    }

    /// The verdict given is true of the clock, for any stalls, whatever the clock's rate
    /// and whether it behaves like QEMU or a real part (the proof's central property:
    /// CLAUDE.md §72).
    #[test]
    fn verdicts_are_never_wrong_whatever_the_stalls() {
        let mut rng = Rng::new(0x5254_4333);
        let mut seen = [0usize; 3]; // confirmed, refuted, undecided
        for case in 0..crate::testing::cases(4000) {
            // A clock from a tenth of the expected rate to ten times it (in parts per
            // ten thousand, to reach the 1 % limit closely).
            let parts = match rng.below(3) {
                0 => 9_900 + (rng.next() % 200), // around the limits
                1 => 9_500 + (rng.next() % 1_000),
                _ => 1_000 + (rng.next() % 99_000),
            };
            let period = HZ / 10_000 * parts;
            let clock = if rng.below(2) == 0 {
                qemu_clock(period, &mut rng)
            } else {
                hardware_clock(period, &mut rng)
            };
            let stalls = match rng.below(6) {
                0 => NO_STALLS,
                1 => Stalls {
                    one_in: 2000,
                    max: 5 * MS,
                    ..NO_STALLS
                },
                2 => Stalls {
                    one_in: 200,
                    max: 10 * MS,
                    ..NO_STALLS
                },
                3 => Stalls {
                    one_in: 50,
                    max: 30 * MS,
                    ..NO_STALLS
                },
                4 => Stalls {
                    at_edge_one_in: 2,
                    at_edge_max: 40 * MS,
                    ..NO_STALLS
                },
                _ => Stalls {
                    one_in: 500,
                    max: 1500 * MS,
                    ..NO_STALLS
                },
            };
            let proof = prove(&clock, HZ, stalls, &mut rng);
            let tolerable = (9900..=10_100).contains(&parts);
            let measured = proof.decided.or(proof.tightest);
            if let Some(m) = measured {
                assert!(
                    (m.min_ticks..=m.max_ticks).contains(&clock.period),
                    "case {case}: {m:?} does not contain the clock's period {}",
                    clock.period
                );
            }
            match proof.decided.map(|m| m.verdict) {
                Some(SecondVerdict::Confirmed) => {
                    seen[0] += 1;
                    assert!(tolerable, "case {case}: {proof:?} confirms parts {parts}");
                }
                Some(SecondVerdict::Refuted) => {
                    seen[1] += 1;
                    assert!(!tolerable, "case {case}: {proof:?} refutes parts {parts}");
                }
                Some(SecondVerdict::Undecided) => unreachable!("a deciding measurement"),
                None => seen[2] += 1,
            }
        }
        // All three outcomes were reached many times.
        assert!(
            seen.iter().all(|&n| n > 300),
            "confirmed/refuted/undecided: {seen:?}"
        );
    }

    /// A clock that does not advance, or whose update bit is stuck, never passes and is
    /// never refuted as a wrong rate: it simply never produces two updates.
    #[test]
    fn a_frozen_clock_gives_no_measurement_at_all() {
        let mut rng = Rng::new(0x5254_4334);
        for case in 0..crate::testing::cases(300) {
            let mut clock = hardware_clock(HZ, &mut rng);
            clock.first = u64::MAX / 2; // no update ever ends
            if rng.below(2) == 0 {
                clock.uip = u64::MAX / 4; // the bit is stuck: every reading is refused
            }
            let stalls = Stalls {
                one_in: 40,
                max: 20 * MS,
                ..NO_STALLS
            };
            let proof = prove(&clock, HZ, stalls, &mut rng);
            assert_eq!(proof.edges, 0, "case {case}");
            assert_eq!(proof.decided.map(|m| m.verdict), None, "case {case}");
            assert_eq!(proof.tightest, None, "case {case}");
        }
    }

    /// Wrong rates, gross and fine, are refuted with the clock and processor running
    /// normally: the clock runs at the wrong rate, the proof sees it.
    #[test]
    fn a_wrong_rate_is_refuted_and_a_good_one_confirmed_without_stalls() {
        let mut rng = Rng::new(0x5254_4335);
        for case in 0..crate::testing::cases(500) {
            let clock = if rng.below(2) == 0 {
                qemu_clock(HZ, &mut rng)
            } else {
                hardware_clock(HZ, &mut rng)
            };
            let good = prove(&clock, HZ, NO_STALLS, &mut rng);
            assert_eq!(
                good.decided.map(|m| m.verdict),
                Some(SecondVerdict::Confirmed),
                "case {case}: {good:?}"
            );
            assert_eq!(
                good.edges, 2,
                "case {case}: it stops at the second edge: {good:?}"
            );
            assert_eq!(good.decided.map(|m| m.seconds), Some(1), "case {case}");
            // Within 1 ms of the model's true second, on these polls.
            let ticks = good.decided.map(|m| m.ticks()).unwrap();
            assert!(ticks.abs_diff(clock.period) < MS, "case {case}: {good:?}");
            assert!(good.spent <= 2 * HZ + 5 * MS, "case {case}: {good:?}");
            // An RTC with the wrong rate, at each of these periods, is refuted.
            for (period, name) in [
                (HZ / 2, "double speed"),
                (HZ + 30 * MS, "3 % slow"),
                (HZ - 30 * MS, "3 % fast"),
                (HZ + 12 * MS, "1.2 % slow"),
                (HZ - 12 * MS, "1.2 % fast"),
            ] {
                let mut wrong = clock;
                wrong.period = period;
                let proof = prove(&wrong, HZ, NO_STALLS, &mut rng);
                assert_eq!(
                    proof.decided.map(|m| m.verdict),
                    Some(SecondVerdict::Refuted),
                    "case {case}: {name}: {proof:?}"
                );
            }
            // A clock too slow to show two updates in the budget is not confirmed: either
            // no measurement at all (the boot fails on the missing updates), or refuted.
            for (period, name) in [(2 * HZ, "half speed"), (5 * HZ, "a fifth of the speed")] {
                let mut slow = clock;
                slow.period = period;
                slow.first = rng.next() % period + 1;
                let proof = prove(&slow, HZ, NO_STALLS, &mut rng);
                assert_ne!(
                    proof.decided.map(|m| m.verdict),
                    Some(SecondVerdict::Confirmed),
                    "case {case}: {name}: {proof:?}"
                );
            }
        }
    }

    /// Stalls of the size measured on the loaded development host (a virtual CPU
    /// descheduled for up to about 5 ms, across one update in three) never cost the proof
    /// a third of a second more than one extra edge: a good clock is confirmed every
    /// time, by its second edge, or its third when two wide edges made the first pair
    /// too vague.
    #[test]
    fn stalls_across_updates_cost_at_most_one_more_edge() {
        let mut rng = Rng::new(0x5254_4336);
        let (mut at_two, mut at_three) = (0usize, 0usize);
        for case in 0..crate::testing::cases(1500) {
            let period = HZ / 1000 * (998 + (rng.next() % 5)); // within ±0.2 %
            let clock = if rng.below(2) == 0 {
                qemu_clock(period, &mut rng)
            } else {
                hardware_clock(period, &mut rng)
            };
            let stalls = Stalls {
                at_edge_one_in: 3,
                at_edge_max: 5 * MS,
                ..NO_STALLS
            };
            let proof = prove(&clock, HZ, stalls, &mut rng);
            assert_eq!(
                proof.decided.map(|m| m.verdict),
                Some(SecondVerdict::Confirmed),
                "case {case}: {proof:?}"
            );
            assert!(proof.spent <= BUDGET, "case {case}: {proof:?}");
            match proof.edges {
                2 => at_two += 1,
                3 => at_three += 1,
                other => panic!("case {case}: {other} edges: {proof:?}"),
            }
        }
        // The common path is the first pair; the third edge is the rare fallback.
        assert!(
            at_two * 100 >= (at_two + at_three) * 95,
            "{at_two} two, {at_three} three"
        );
    }

    /// The precise failure this was written for: with the update-in-progress pulse
    /// missed at both edges (a virtual CPU descheduled across every one of QEMU's
    /// 244 µs windows), the old wait for the bit expired; this proof still decides.
    #[test]
    fn missing_every_update_in_progress_pulse_is_harmless() {
        let mut rng = Rng::new(0x5254_4338);
        for case in 0..crate::testing::cases(500) {
            let clock = qemu_clock(HZ, &mut rng);
            // Every update's end falls in a stall of 2 ms: the polls never see the bit.
            let stalls = Stalls {
                at_edge_one_in: 1,
                at_edge_max: 2 * MS,
                ..NO_STALLS
            };
            let proof = prove(&clock, HZ, stalls, &mut rng);
            assert_eq!(
                proof.decided.map(|m| m.verdict),
                Some(SecondVerdict::Confirmed),
                "case {case}: {proof:?}"
            );
            assert_eq!(proof.edges, 2, "case {case}: {proof:?}");
        }
    }

    /// A stream of polls with arbitrary values, in any order: nothing panics, and any
    /// measurement produced is consistent with itself (CLAUDE.md §13).
    #[test]
    fn any_polls_are_handled() {
        let mut rng = Rng::new(0x5254_4337);
        for case in 0..crate::testing::cases(20_000) {
            let mut w = UpdateWatcher::new(rng.byte(), rng.edge_u64());
            let mut m = SecondMeter::new(rng.edge_u64(), rng.below(30_000) as u16);
            let mut last = None;
            for _ in 0..rng.below(16) {
                let p = Poll {
                    before: rng.edge_u64(),
                    after: rng.edge_u64(),
                    updating: rng.below(3) == 0,
                    seconds: rng.byte(),
                };
                if let Some(edge) = w.observe(p) {
                    last = m.add(edge).or(last);
                    assert!(edge.count > 0, "case {case}");
                }
            }
            for measured in [last, m.tightest()].into_iter().flatten() {
                assert!(measured.min_ticks <= measured.max_ticks, "case {case}");
                assert!(measured.seconds > 0, "case {case}");
                assert!(
                    (measured.min_ticks..=measured.max_ticks).contains(&measured.ticks()),
                    "case {case}"
                );
                assert!(
                    measured.ticks() + measured.error_ticks() >= measured.max_ticks,
                    "case {case}"
                );
            }
        }
    }
}
