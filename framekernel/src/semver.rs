//! Semantic version parsing and precedence (semver.org 2.0.0; CLAUDE.md §46 dependency
//! resolution). Parses `MAJOR.MINOR.PATCH[-prerelease][+build]` and orders versions by the
//! spec's precedence rules: numeric core compared field by field, a prerelease ranked below
//! the matching normal version, prerelease identifiers compared numerically or lexically
//! (numeric below alphanumeric), and build metadata ignored for ordering. The package
//! manager needs this to pick which version satisfies a dependency. Pure, no `unsafe`.

extern crate alloc;
use alloc::string::String;
use alloc::vec::Vec;

/// One prerelease identifier: all-digits are numeric (compared by value), otherwise
/// alphanumeric (compared lexically); numeric ranks below alphanumeric.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Identifier {
    Numeric(u64),
    Alpha(String),
}

impl Ord for Identifier {
    fn cmp(&self, other: &Self) -> core::cmp::Ordering {
        use Identifier::{Alpha, Numeric};
        match (self, other) {
            (Numeric(a), Numeric(b)) => a.cmp(b),
            (Alpha(a), Alpha(b)) => a.cmp(b),
            (Numeric(_), Alpha(_)) => core::cmp::Ordering::Less,
            (Alpha(_), Numeric(_)) => core::cmp::Ordering::Greater,
        }
    }
}
impl PartialOrd for Identifier {
    fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// A parsed semantic version. Build metadata is kept for display but ignored in ordering
/// and equality (two versions differing only in build metadata are equal, per §10).
#[derive(Clone, Debug)]
pub struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
    pre: Vec<Identifier>,
    pub build: Option<String>,
}

impl Version {
    /// Whether this is a prerelease (has prerelease identifiers).
    pub fn is_prerelease(&self) -> bool {
        !self.pre.is_empty()
    }
}

/// A numeric identifier: digits only, and no leading zero unless it is exactly `0`.
fn parse_numeric(s: &str) -> Option<u64> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    if s.len() > 1 && s.starts_with('0') {
        return None; // no leading zeros
    }
    s.parse().ok()
}

/// A prerelease/build identifier character set: ASCII alphanumerics and hyphen.
fn is_identifier(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

/// Parses a semantic version string, or `None` if it is not valid semver.
pub fn parse(input: &str) -> Option<Version> {
    // Split off build metadata (`+...`), then prerelease (`-...`).
    let (rest, build) = match input.split_once('+') {
        Some((r, b)) => {
            if !b.split('.').all(is_identifier) {
                return None;
            }
            (r, Some(String::from(b)))
        }
        None => (input, None),
    };
    let (core, pre_str) = match rest.split_once('-') {
        Some((c, p)) => (c, Some(p)),
        None => (rest, None),
    };

    let mut fields = core.split('.');
    let major = parse_numeric(fields.next()?)?;
    let minor = parse_numeric(fields.next()?)?;
    let patch = parse_numeric(fields.next()?)?;
    if fields.next().is_some() {
        return None; // too many core fields
    }

    let mut pre = Vec::new();
    if let Some(p) = pre_str {
        for id in p.split('.') {
            if !is_identifier(id) {
                return None;
            }
            // A numeric identifier parses as a number (rejecting leading zeros); otherwise
            // it is alphanumeric.
            pre.push(match parse_numeric(id) {
                Some(n) => Identifier::Numeric(n),
                None if id.bytes().all(|b| b.is_ascii_digit()) => return None, // e.g. "01"
                None => Identifier::Alpha(String::from(id)),
            });
        }
    }

    Some(Version {
        major,
        minor,
        patch,
        pre,
        build,
    })
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> core::cmp::Ordering {
        use core::cmp::Ordering::Equal;
        (self.major, self.minor, self.patch)
            .cmp(&(other.major, other.minor, other.patch))
            .then_with(|| match (self.pre.is_empty(), other.pre.is_empty()) {
                (true, true) => Equal,
                (true, false) => core::cmp::Ordering::Greater, // normal > prerelease
                (false, true) => core::cmp::Ordering::Less,
                (false, false) => self.pre.cmp(&other.pre), // element-wise, shorter first
            })
    }
}
impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl PartialEq for Version {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == core::cmp::Ordering::Equal
    }
}
impl Eq for Version {}

impl Version {
    /// A normal (no-prerelease, no-build) version, for computing range bounds.
    fn normal(major: u64, minor: u64, patch: u64) -> Version {
        Version {
            major,
            minor,
            patch,
            pre: Vec::new(),
            build: None,
        }
    }

    fn same_core(&self, other: &Version) -> bool {
        (self.major, self.minor, self.patch) == (other.major, other.minor, other.patch)
    }
}

/// A single comparator's operator.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Op {
    Exact,
    Greater,
    GreaterEq,
    Less,
    LessEq,
    /// `^`: compatible within the left-most non-zero field.
    Caret,
    /// `~`: patch-level changes within the minor.
    Tilde,
}

#[derive(Debug)]
struct Comparator {
    op: Op,
    base: Version,
}

impl Comparator {
    /// The exclusive upper bound of a caret/tilde comparator.
    fn upper(&self) -> Version {
        let b = &self.base;
        match self.op {
            Op::Tilde => Version::normal(b.major, b.minor + 1, 0),
            Op::Caret if b.major > 0 => Version::normal(b.major + 1, 0, 0),
            Op::Caret if b.minor > 0 => Version::normal(0, b.minor + 1, 0),
            Op::Caret => Version::normal(0, 0, b.patch + 1),
            _ => b.clone(),
        }
    }

    fn matches(&self, v: &Version) -> bool {
        // Prerelease gate (the Cargo/npm convention): a prerelease version may only match
        // when the comparator's own base is a prerelease on the same core — so `1.2.4-rc`
        // never slips into `^1.2.3`, and `2.0.0-rc` never into `<2.0.0`.
        if v.is_prerelease() && !(self.base.is_prerelease() && v.same_core(&self.base)) {
            return false;
        }
        match self.op {
            Op::Exact => v == &self.base,
            Op::Greater => v > &self.base,
            Op::GreaterEq => v >= &self.base,
            Op::Less => v < &self.base,
            Op::LessEq => v <= &self.base,
            Op::Caret | Op::Tilde => v >= &self.base && v < &self.upper(),
        }
    }
}

/// A version requirement: a conjunction (AND) of comma-separated comparators, e.g.
/// `">=1.2.0, <2.0.0"` or `"^1.4.2"`. A bare version (no operator) is exact.
#[derive(Debug)]
pub struct Req {
    comparators: Vec<Comparator>,
}

/// Parses a requirement string, or `None` if any comparator is malformed.
pub fn parse_req(input: &str) -> Option<Req> {
    let mut comparators = Vec::new();
    for part in input.split(',') {
        let part = part.trim();
        if part.is_empty() {
            return None;
        }
        let (op, rest) = if let Some(r) = part.strip_prefix(">=") {
            (Op::GreaterEq, r)
        } else if let Some(r) = part.strip_prefix("<=") {
            (Op::LessEq, r)
        } else if let Some(r) = part.strip_prefix('>') {
            (Op::Greater, r)
        } else if let Some(r) = part.strip_prefix('<') {
            (Op::Less, r)
        } else if let Some(r) = part.strip_prefix('^') {
            (Op::Caret, r)
        } else if let Some(r) = part.strip_prefix('~') {
            (Op::Tilde, r)
        } else if let Some(r) = part.strip_prefix('=') {
            (Op::Exact, r)
        } else {
            (Op::Exact, part)
        };
        comparators.push(Comparator {
            op,
            base: parse(rest.trim())?,
        });
    }
    if comparators.is_empty() {
        return None;
    }
    Some(Req { comparators })
}

impl Req {
    /// Whether `version` satisfies every comparator.
    pub fn matches(&self, version: &Version) -> bool {
        self.comparators.iter().all(|c| c.matches(version))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Version {
        parse(s).unwrap_or_else(|| panic!("parse {s:?}"))
    }

    #[test]
    fn parses_fields_prerelease_and_build() {
        let x = v("1.2.3");
        assert_eq!((x.major, x.minor, x.patch), (1, 2, 3));
        assert!(!x.is_prerelease());
        let y = v("10.20.30-alpha.1+build.5");
        assert_eq!((y.major, y.minor, y.patch), (10, 20, 30));
        assert!(y.is_prerelease());
        assert_eq!(y.build.as_deref(), Some("build.5"));
        // Build metadata does not affect equality (§10).
        assert_eq!(v("1.0.0+a"), v("1.0.0+b"));
        assert_eq!(v("1.0.0"), v("1.0.0+exp.sha.5114f85"));
    }

    #[test]
    fn rejects_invalid_versions() {
        for bad in [
            "1",
            "1.2",
            "1.2.3.4",
            "01.0.0",
            "1.00.0",
            "1.2.x",
            "",
            "1.2.3-",
            "1.2.3-01",
            "1.2.3-+b",
            "1.2.3 ",
            "v1.2.3",
            "1.2.-3",
            "1.2.3-beta_1",
        ] {
            assert!(parse(bad).is_none(), "{bad:?} must be rejected");
        }
    }

    #[test]
    fn precedence_follows_the_spec_example() {
        // The canonical chain from semver.org §11.
        let chain = [
            "1.0.0-alpha",
            "1.0.0-alpha.1",
            "1.0.0-alpha.beta",
            "1.0.0-beta",
            "1.0.0-beta.2",
            "1.0.0-beta.11",
            "1.0.0-rc.1",
            "1.0.0",
        ];
        for w in chain.windows(2) {
            assert!(v(w[0]) < v(w[1]), "{} should precede {}", w[0], w[1]);
            assert!(v(w[1]) > v(w[0]));
        }
        // Core precedence and prerelease-below-normal.
        assert!(v("1.0.0") < v("2.0.0"));
        assert!(v("2.0.0") < v("2.1.0"));
        assert!(v("2.1.0") < v("2.1.1"));
        assert!(v("1.0.0-alpha") < v("1.0.0"));
        // A longer prerelease with an equal prefix has higher precedence.
        assert!(v("1.0.0-alpha") < v("1.0.0-alpha.1"));
        // Numeric identifiers rank below alphanumeric ones.
        assert!(v("1.0.0-1") < v("1.0.0-alpha"));
        // Build metadata is ignored in ordering.
        assert_eq!(v("1.0.0+a").cmp(&v("1.0.0+b")), core::cmp::Ordering::Equal);
    }

    #[test]
    fn sorting_a_list_orders_by_precedence() {
        let mut list: Vec<Version> = ["1.0.0", "1.0.0-rc.1", "0.9.9", "1.0.1", "1.0.0-alpha"]
            .iter()
            .map(|s| v(s))
            .collect();
        list.sort();
        let sorted: Vec<String> = list
            .iter()
            .map(|x| {
                alloc::format!(
                    "{}.{}.{}{}",
                    x.major,
                    x.minor,
                    x.patch,
                    if x.is_prerelease() { "-pre" } else { "" }
                )
            })
            .collect();
        assert_eq!(
            sorted,
            ["0.9.9", "1.0.0-pre", "1.0.0-pre", "1.0.0", "1.0.1"]
        );
    }

    fn req(s: &str) -> Req {
        parse_req(s).unwrap_or_else(|| panic!("parse_req {s:?}"))
    }

    #[test]
    fn caret_tilde_and_range_requirements() {
        // Caret: compatible within the left-most non-zero field.
        let r = req("^1.2.3");
        assert!(r.matches(&v("1.2.3")) && r.matches(&v("1.2.9")) && r.matches(&v("1.9.0")));
        assert!(!r.matches(&v("1.2.2")) && !r.matches(&v("2.0.0")));
        assert!(req("^0.2.3").matches(&v("0.2.9")) && !req("^0.2.3").matches(&v("0.3.0")));
        assert!(req("^0.0.3").matches(&v("0.0.3")) && !req("^0.0.3").matches(&v("0.0.4")));
        // Tilde: patch-level within the minor.
        let t = req("~1.2.3");
        assert!(t.matches(&v("1.2.3")) && t.matches(&v("1.2.99")));
        assert!(!t.matches(&v("1.3.0")) && !t.matches(&v("1.2.2")));
        // A comma-separated range (AND) and explicit comparators.
        let range = req(">=1.0.0, <2.0.0");
        assert!(
            range.matches(&v("1.5.0"))
                && !range.matches(&v("2.0.0"))
                && !range.matches(&v("0.9.9"))
        );
        assert!(req("=1.2.3").matches(&v("1.2.3")) && !req("=1.2.3").matches(&v("1.2.4")));
        assert!(req("1.2.3").matches(&v("1.2.3")), "a bare version is exact");
    }

    #[test]
    fn prerelease_only_matches_when_opted_in() {
        // A prerelease never slips into a normal requirement, even inside its range.
        assert!(
            !req("^1.2.3").matches(&v("1.2.4-rc.1")),
            "different core prerelease"
        );
        assert!(
            !req(">=1.0.0, <2.0.0").matches(&v("2.0.0-rc.1")),
            "upper-bound prerelease"
        );
        assert!(!req(">=1.0.0").matches(&v("1.5.0-alpha")));
        // It matches only when a comparator names the same core and is itself a prerelease.
        assert!(req("^1.2.3-rc.1").matches(&v("1.2.3-rc.1")));
        assert!(req("^1.2.3-rc.1").matches(&v("1.2.3-rc.2")));
        assert!(
            req("^1.2.3-rc.1").matches(&v("1.2.3")),
            "the normal release is in range"
        );
        assert!(
            !req("^1.2.3-rc.5").matches(&v("1.2.3-rc.1")),
            "below the floor"
        );
        assert!(req(">=1.2.3-rc.1").matches(&v("1.2.3-rc.1")));
    }

    #[test]
    fn rejects_malformed_requirements() {
        for bad in [
            "",
            "   ",
            ">=",
            "^1.2",
            ">= 1.0.0, ",
            "~bad",
            "1.2.3 1.4.0",
            ">1",
        ] {
            assert!(parse_req(bad).is_none(), "{bad:?} must be rejected");
        }
    }
}
