//! The one compiler warning `cargo xtask` expects, collapsed to a single line.
//!
//! The kernel is built with `-C target-feature=+harden-sls-ret` (ADR-0017 Z10; `.cargo/config.toml`).
//! Stable Rust 1.99.1 has no supported option for it: the compiler passes the feature to the code
//! generator and warns, once for every crate it compiles for the kernel's target, that the feature
//! is unknown and unstable. No lint governs that warning, so `-A` cannot silence it. The image
//! gate (`image.rs`) is what holds the protection: it reads every return in the built kernel.
//!
//! The owner chose, on 2026-11-12, to see that warning as one line instead of one per crate.
//! [`Filter`] drops exactly this:
//!
//! - the warning's own five lines, only when all five match the text below;
//! - a crate's summary line, `warning: `name` (lib) generated 1 warning`, with or without
//!   ` (1 duplicate)`, only while every warning of the build so far has been this one.
//!
//! Every other line passes through unchanged and in order. Once any other warning appears, no
//! summary line is dropped for the rest of that command, so the counts stay visible. Lines are
//! matched with their colour codes removed and printed as they came.

/// The warning's lines, without colour codes, as the compiler prints them.
const BLOCK: [&str; 5] = [
    "warning: unknown and unstable feature specified for `-Ctarget-feature`: `harden-sls-ret`",
    "  |",
    "  = note: it is still passed through to the codegen backend, but use of this feature might be unsound and the behavior of this feature can change in the future",
    "  = help: consider filing a feature request",
    "",
];

/// The line `cargo xtask` prints in place of what [`Filter`] dropped.
pub(crate) fn note(dropped: &Dropped) -> String {
    format!(
        "xtask: note: the compiler's warning that `-C target-feature=+harden-sls-ret` is unknown \
         and unstable is expected (ADR-0017 Z10) and was collapsed (warnings: {}, crate \
         summaries: {}); the image gate in `cargo xtask check` holds the protection",
        dropped.blocks, dropped.summaries
    )
}

/// What a [`Filter`] dropped.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Dropped {
    pub(crate) blocks: usize,
    pub(crate) summaries: usize,
}

/// Reads a command's error output line by line and gives back the lines to print.
#[derive(Debug, Default)]
pub(crate) struct Filter {
    /// Lines held while they match the start of [`BLOCK`].
    held: Vec<String>,
    /// A warning other than the expected one has been seen.
    other_warning: bool,
    dropped: Dropped,
}

impl Filter {
    /// Takes one line, without its line ending, and returns the lines to print now.
    pub(crate) fn line(&mut self, raw: &str) -> Vec<String> {
        let plain = strip_colour(raw);
        let next = self.held.len();
        if plain == BLOCK[next] {
            self.held.push(raw.to_owned());
            if self.held.len() == BLOCK.len() {
                self.held.clear();
                self.dropped.blocks += 1;
            }
            return Vec::new();
        }
        // Not the expected block's next line: what was held passes through, and this line is
        // looked at afresh (it may start the block itself).
        let mut out = std::mem::take(&mut self.held);
        if !out.is_empty() {
            self.other_warning = true;
        }
        if plain == BLOCK[0] {
            self.held.push(raw.to_owned());
            return out;
        }
        if is_summary_of_one(&plain) {
            if self.dropped.blocks > 0 && !self.other_warning {
                self.dropped.summaries += 1;
                return out;
            }
        } else if is_diagnostic_header(&plain) {
            self.other_warning = true;
        }
        out.push(raw.to_owned());
        out
    }

    /// Ends the output: returns what was still held, and what was dropped.
    pub(crate) fn finish(mut self) -> (Vec<String>, Dropped) {
        let rest = std::mem::take(&mut self.held);
        (rest, self.dropped)
    }
}

/// `warning: `name` (lib) generated 1 warning`, for a library, a binary or a test, with or
/// without ` (1 duplicate)`.
fn is_summary_of_one(plain: &str) -> bool {
    let Some(rest) = plain.strip_prefix("warning: `") else {
        return false;
    };
    let Some((_name, rest)) = rest.split_once("` (") else {
        return false;
    };
    rest.ends_with(") generated 1 warning") || rest.ends_with(") generated 1 warning (1 duplicate)")
}

/// The first line of a compiler diagnostic: `warning: …`, `warning[…]: …`, `error: …` or
/// `error[…]: …`, but not a crate's summary line.
fn is_diagnostic_header(plain: &str) -> bool {
    let kind = ["warning", "error"]
        .into_iter()
        .find(|k| plain.starts_with(k));
    let Some(kind) = kind else { return false };
    let rest = &plain[kind.len()..];
    let starts = rest.starts_with(": ") || rest.starts_with('[');
    starts && !(plain.starts_with("warning: `") && plain.contains(") generated "))
}

/// The line without its ANSI colour and style codes (`ESC [ … letter`).
fn strip_colour(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' && chars.peek() == Some(&'[') {
            chars.next();
            for c in chars.by_ref() {
                if c.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(lines: &[&str]) -> (Vec<String>, Dropped) {
        let mut filter = Filter::default();
        let mut out = Vec::new();
        for line in lines {
            out.extend(filter.line(line));
        }
        let (rest, dropped) = filter.finish();
        out.extend(rest);
        (out, dropped)
    }

    /// The output CI printed on 2026-11-12 (run 43118681550), cut down: the warning once, then
    /// one summary for each crate.
    #[test]
    fn the_expected_warning_and_its_summaries_are_dropped() {
        let mut input = vec![
            "   Compiling origin-version v0.0.0 (/w/runtime/version)",
            "   Compiling origin-sync v0.0.0 (/w/framekernel/frame/sync)",
        ];
        input.extend(BLOCK);
        input.extend([
            "   Compiling origin-syscalls v0.0.0 (/w/framekernel/syscalls)",
            "warning: `origin-version` (lib) generated 1 warning",
            "warning: `origin-sync` (lib) generated 1 warning (1 duplicate)",
            "warning: `framekernel` (bin \"framekernel\") generated 1 warning (1 duplicate)",
            "    Finished `release` profile [optimized] target(s) in 9.87s",
        ]);
        let (out, dropped) = run(&input);
        assert_eq!(
            out,
            [
                "   Compiling origin-version v0.0.0 (/w/runtime/version)",
                "   Compiling origin-sync v0.0.0 (/w/framekernel/frame/sync)",
                "   Compiling origin-syscalls v0.0.0 (/w/framekernel/syscalls)",
                "    Finished `release` profile [optimized] target(s) in 9.87s",
            ]
        );
        assert_eq!(
            dropped,
            Dropped {
                blocks: 1,
                summaries: 3
            }
        );
    }

    /// Any other warning passes through whole, and from then on no summary is dropped.
    #[test]
    fn another_warning_and_every_later_summary_pass_through() {
        let mut input: Vec<&str> = BLOCK.to_vec();
        input.extend([
            "warning: `origin-version` (lib) generated 1 warning",
            "warning: unused variable: `x`",
            " --> framekernel/src/lib.rs:1:5",
            "",
            "warning: `framekernel` (lib) generated 1 warning",
            "warning: `origin-sync` (lib) generated 1 warning (1 duplicate)",
        ]);
        let (out, dropped) = run(&input);
        assert_eq!(
            out,
            [
                "warning: unused variable: `x`",
                " --> framekernel/src/lib.rs:1:5",
                "",
                "warning: `framekernel` (lib) generated 1 warning",
                "warning: `origin-sync` (lib) generated 1 warning (1 duplicate)",
            ]
        );
        assert_eq!(
            dropped,
            Dropped {
                blocks: 1,
                summaries: 1
            }
        );
    }

    /// A crate with two warnings keeps its summary, and so do summaries with no expected
    /// warning before them.
    #[test]
    fn other_counts_and_summaries_without_the_warning_pass_through() {
        let (out, dropped) = run(&[
            "warning: `origin-sync` (lib) generated 1 warning",
            "warning: `framekernel` (lib) generated 2 warnings (1 duplicate)",
        ]);
        assert_eq!(out.len(), 2);
        assert_eq!(dropped, Dropped::default());
    }

    /// The warning's first line with anything else after it is not the expected warning: every
    /// line passes through, in order.
    #[test]
    fn a_partial_match_passes_through_in_order() {
        let input = [
            BLOCK[0],
            BLOCK[1],
            "  = note: something the compiler did not say before",
            "",
            "warning: `framekernel` (lib) generated 1 warning",
        ];
        let (out, dropped) = run(&input);
        assert_eq!(out, input);
        assert_eq!(dropped, Dropped::default());
        // Cut off at the end: what was held still comes out.
        let (out, _) = run(&BLOCK[..3]);
        assert_eq!(out, &BLOCK[..3]);
    }

    /// The same lines in colour, as cargo prints them to a terminal.
    #[test]
    fn colour_codes_are_ignored_when_matching() {
        let header = "\u{1b}[0m\u{1b}[1m\u{1b}[33mwarning\u{1b}[0m\u{1b}[0m\u{1b}[1m: unknown and unstable feature specified for `-Ctarget-feature`: `harden-sls-ret`\u{1b}[0m";
        let pipe = "\u{1b}[0m  \u{1b}[0m\u{1b}[0m\u{1b}[1m\u{1b}[38;5;12m|\u{1b}[0m";
        let summary = "\u{1b}[1m\u{1b}[33mwarning\u{1b}[0m\u{1b}[1m:\u{1b}[0m `origin-abi` (lib) generated 1 warning (1 duplicate)";
        let (out, dropped) = run(&[header, pipe, BLOCK[2], BLOCK[3], BLOCK[4], summary]);
        assert!(out.is_empty(), "{out:?}");
        assert_eq!(
            dropped,
            Dropped {
                blocks: 1,
                summaries: 1
            }
        );
    }

    #[test]
    fn the_note_names_the_option_and_the_gate() {
        let n = note(&Dropped {
            blocks: 1,
            summaries: 12,
        });
        assert!(
            n.contains("harden-sls-ret") && n.contains("ADR-0017 Z10") && n.contains("image gate")
        );
        assert!(n.contains("warnings: 1") && n.contains("summaries: 12"));
    }
}
