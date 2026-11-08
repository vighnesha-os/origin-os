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
}
