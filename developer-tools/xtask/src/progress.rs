//! `cargo xtask progress`, which writes `PROGRESS.md` at the repository root (owner item 356;
//! `docs/PROGRESS.md` until TASKS item 137), and the gate in `cargo xtask check`
//! that keeps it current (TASKS item 43).
//!
//! The owner asked for "clear progress status table, which need to be updated for every PR is
//! merged". The page is generated from the requirements matrix (`docs/requirements/`), the
//! headings of CLAUDE.md and of the ADRs, and the placements and state words of
//! `docs/ROADMAP.md`. CI runs `check` on every pull request, and the gate fails when the
//! committed page differs by a byte from the one the tree generates, so a pull request cannot
//! merge with a stale page.
//!
//! **What the page claims:** counts of rows of the matrix, each row counted once, whatever its
//! state (no weighting), and for each line the four states' shares, which add up to 100.0%
//! (owner item 357: no figure adds two states together; only done counts as complete, decision
//! items 193 and 221). Done's share is the requirements gate's own figure, rounded as the gate
//! rounds it. Nothing on it depends on the clock, the host, a path or a commit hash, so
//! one tree gives one page (CLAUDE.md §75).
//!
//! **Its layout** (TASKS item 137): a title and one line; the overall table; what the states
//! mean; where the evidence ran; the milestones; the feature areas; the details, each closed
//! until a reader opens it; and how the page is made. A reader sees "requirements" and "feature
//! areas"; "rows" and "groups" are this module's words for the same things.
//!
//! **What it refuses, failing closed:** a matrix it cannot read; a roadmap the roadmap gate
//! refuses; a milestone that gives no state word, or two; a section of a feature group with no
//! heading in CLAUDE.md; an ADR of the matrix with no title; a page with no row counted.
//!
//! **What it does not see:** whether a row's state is the right judgement, which the requirements
//! gate checks as far as the tree allows, and whether a placement is right.
//!
//! It reads the matrix through the requirements gate's reader and the roadmap through the roadmap
//! gate's ([`requirements::matrix_rows`], [`roadmap::check`], [`roadmap::states`]): there is one
//! reader of each.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::fs;

use crate::requirements::{self, Env, Kind, Row, State};
use crate::roadmap;
use crate::specifications::{self, Spec};
use crate::{Result, repo_root};

/// The page this module writes and its gate reads.
const PAGE: &str = "PROGRESS.md";
/// Where the page was until TASKS item 137 (owner item 356). A copy left there is refused
/// ([`no_copy_at_the_old_place`]).
const OLD_PAGE: &str = "docs/PROGRESS.md";
/// The command that writes it.
const COMMAND: &str = "cargo xtask progress";

/// The feature groups of `build` rows, by directive section: a name, the first section and the
/// last. The coordinating session's brief for TASKS item 43 gives them; the sections' titles come
/// from CLAUDE.md's headings.
const GROUPS: [(&str, u8, u8); 14] = [
    ("Toolchain, build and CI", 8, 15),
    ("Boot, installer and first-run setup", 19, 23),
    ("Kernel core", 24, 30),
    ("Devices", 31, 38),
    ("Storage and filesystem", 39, 41),
    ("Native applications", 42, 47),
    ("SDK and the SDK-only rule", 48, 49),
    ("GUI", 50, 53),
    ("Networking, audio and peripherals", 54, 56),
    ("Accounts, security, cryptography and privacy", 57, 61),
    ("Versions, updates and recovery", 62, 65),
    ("Performance, power, time and reliability", 66, 69),
    ("Developer platform", 70, 71),
    ("Testing, hardware qualification and release", 72, 76),
];

/// The four states, in the page's order, each with what it means in one plain sentence (owner
/// item 357; the coordinating session's contract for TASKS item 137 gives the sentences, and its
/// ruling of 2026-11-23 the one for built: a requirement is built for either of two reasons, and
/// the contract's first sentence gave one. The review of TASKS item 137, R137-2).
const MEANINGS: [(&str, &str); 4] = [
    (
        "Done",
        "Its acceptance tests pass in the environment the requirement asks for.",
    ),
    (
        "Built",
        "It is written and tested, but not all of its acceptance tests pass yet, or they pass only in a weaker environment than it asks for, such as a virtual machine where it asks for a real one.",
    ),
    (
        "In progress",
        "Part of it is written and tested, or something is written with no test evidence yet.",
    ),
    ("Not started", "Nothing for it is in the tree."),
];

/// What a line of a table counts: rows in each of the four states.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Tally {
    done: usize,
    built: usize,
    /// `partial` and `not-verified`.
    in_progress: usize,
    not_started: usize,
}

impl Tally {
    /// Counts `row` if it is counted: not `info`, not `removed`.
    fn add(&mut self, row: &Row) {
        match counted(row) {
            Some(State::Done) => self.done += 1,
            Some(State::Built) => self.built += 1,
            Some(State::Partial | State::NotVerified) => self.in_progress += 1,
            Some(State::NotStarted) => self.not_started += 1,
            Some(State::Removed) | None => {}
        }
    }

    fn of<'a>(rows: impl IntoIterator<Item = &'a Row>) -> Self {
        let mut tally = Self::default();
        for row in rows {
            tally.add(row);
        }
        tally
    }

    fn rows(self) -> usize {
        self.done + self.built + self.in_progress + self.not_started
    }

    /// The four counts, in the page's order: done, built, in progress, not started.
    fn parts(self) -> [usize; 4] {
        [self.done, self.built, self.in_progress, self.not_started]
    }

    /// The four states' shares in tenths of a percent, which sum to exactly 1,000, or `None`
    /// for a line with no requirement (owner item 357: four percentages that add up to 100).
    ///
    /// **Done is rounded to the nearest tenth, from done and the total alone,** exactly as the
    /// requirements gate rounds it ([`requirements::percent`]). It is the one figure that counts
    /// as complete (decision item 193), so it never moves because another state moved, and the
    /// page never disagrees with what `cargo xtask check` prints (the review of TASKS item 137,
    /// R137-1; the coordinating session's ruling of 2026-11-23).
    ///
    /// **The other three share the rest by largest remainder:** each is rounded down, and the
    /// tenths still owed go to the largest remainders, one each. Where two remainders are equal,
    /// the less complete state takes the tenth. Every share is within a tenth of its exact value,
    /// and a state with no requirement has no share. The cost, accepted by that ruling: done and
    /// a state one requirement away from it can be shown a tenth out of order.
    fn shares(self) -> Option<[usize; 4]> {
        let whole = self.rows();
        if whole == 0 {
            return None;
        }
        let parts = self.parts();
        let mut shares = parts.map(|part| part * 1000 / whole);
        // As `requirements::percent`: to the nearest tenth, a half up.
        shares[0] = (self.done * 1000 + whole / 2) / whole;
        let owed = 1000 - shares.iter().sum::<usize>();
        // The other three by remainder, largest first; among equals, the less complete first
        // (the sort is stable).
        let mut order = [3, 2, 1];
        order.sort_by_key(|&state| std::cmp::Reverse(parts[state] * 1000 % whole));
        for &state in order.iter().take(owed) {
            shares[state] += 1;
        }
        Some(shares)
    }

    /// The table cells of a line of percentages: its requirements, then each state's share, or
    /// "-" in each state's cell for a line with no requirement.
    fn share_cells(self) -> String {
        let shares = self.shares().map_or_else(
            || ["-".to_string(), "-".into(), "-".into(), "-".into()],
            |shares| shares.map(tenths),
        );
        format!("{} | {}", count(self.rows()), shares.join(" | "))
    }

    /// The table cells of a line of counts: its requirements, then each state's count.
    fn count_cells(self) -> String {
        format!(
            "{} | {}",
            count(self.rows()),
            self.parts().map(count).join(" | ")
        )
    }
}

/// The header of every table of percentages or of counts, after its first columns.
const STATES: &str = "Requirements | Done | Built | In progress | Not started";
/// Its rule, after the first columns'.
const STATES_RULE: &str = "--:|--:|--:|--:|--:";

/// `tenths` of a percent, as a percentage with one decimal.
fn tenths(tenths: usize) -> String {
    format!("{}.{}%", tenths / 10, tenths % 10)
}

/// A count as the page writes it: a thousand or more with commas.
fn count(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, digit) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

/// `n` of a thing, as the page writes it: "1 requirement", "1,008 requirements".
fn requirements_of(n: usize) -> String {
    format!("{} requirement{}", count(n), if n == 1 { "" } else { "s" })
}

/// The row's state if it is counted: not `info` (no state) and not `removed`.
fn counted(row: &Row) -> Option<State> {
    row.state.filter(|state| *state != State::Removed)
}

/// The numbered section of the directive a row is in, from its ID (`S33-07` is in §33).
fn section_of(row: &Row) -> Option<u8> {
    if !row.file.starts_with("directive-") {
        return None;
    }
    let (prefix, _) = row.id.split_once('-')?;
    prefix.strip_prefix('S')?.parse().ok()
}

/// Where a directive row outside the numbered sections is, as a reader would name it: the
/// directive's header (`H`) or its final rules (`F`); any other by its ID's prefix (`P9`).
fn place_of(row: &Row) -> String {
    match section_of(row) {
        Some(section) => format!("§{section}"),
        None => match row.id.split_once('-').map_or("?", |(p, _)| p) {
            "H" => "the header".to_string(),
            "F" => "the final rules".to_string(),
            other => other.to_string(),
        },
    }
}

/// The ADR a row of `adr-NNNN.md` stands for.
fn adr_of(row: &Row) -> Option<u16> {
    row.file
        .strip_prefix("adr-")?
        .strip_suffix(".md")?
        .parse()
        .ok()
}

/// What the page is made from.
struct Inputs<'a> {
    rows: &'a [Row],
    /// Each numbered section's title, from its heading in CLAUDE.md.
    titles: &'a BTreeMap<u8, String>,
    /// Each ADR's title, and whether it is accepted.
    adrs: &'a BTreeMap<u16, (String, bool)>,
    roadmap: &'a str,
    /// The specifications the project relies on, already checked against the matrix
    /// ([`specifications::read`]).
    specs: &'a [Spec],
}

/// The page, or everything that keeps it from being written.
fn page(inputs: &Inputs<'_>) -> std::result::Result<String, Vec<String>> {
    let rows = inputs.rows;
    let mut problems = Vec::new();
    let all = Tally::of(rows);
    if all.rows() == 0 {
        problems.push("no counted row of the matrix was read: there is nothing to count".into());
    }
    let found = roadmap::check(inputs.roadmap, rows);
    problems.extend(found.problems.iter().cloned());
    let (states, state_problems) = roadmap::states(inputs.roadmap);
    problems.extend(state_problems);

    let mut out = String::new();
    let w = &mut out;

    // 1. The title and one line.
    let _ = writeln!(w, "# Origin OS progress");
    let _ = writeln!(w);
    let _ = writeln!(
        w,
        "**{} of {} are done.** Done is the only state that counts as complete.",
        all.shares()
            .map_or_else(|| "-".to_string(), |s| tenths(s[0])),
        requirements_of(all.rows())
    );
    let _ = writeln!(w);
    let _ = writeln!(
        w,
        "A requirement is one statement of [`CLAUDE.md`](CLAUDE.md), or one decision of an accepted ADR."
    );

    // 2. Overall.
    let _ = writeln!(w);
    let _ = writeln!(w, "## Overall");
    let _ = writeln!(w);
    let _ = writeln!(w, "| State | Requirements | Share |");
    let _ = writeln!(w, "|---|--:|--:|");
    let overall = all.shares().unwrap_or([0; 4]);
    for ((name, _), (part, share)) in MEANINGS.iter().zip(all.parts().into_iter().zip(overall)) {
        let _ = writeln!(w, "| {name} | {} | {} |", count(part), tenths(share));
    }
    let _ = writeln!(
        w,
        "| **Total** | **{}** | **{}** |",
        count(all.rows()),
        tenths(overall.iter().sum())
    );

    // 3. What the states mean.
    let _ = writeln!(w);
    let _ = writeln!(w, "## What the states mean");
    let _ = writeln!(w);
    let _ = writeln!(w, "| State | Meaning |");
    let _ = writeln!(w, "|---|---|");
    for (name, meaning) in MEANINGS {
        let _ = writeln!(w, "| {name} | {meaning} |");
    }

    // 4. Where the evidence ran.
    let mut ran = [0usize; 4];
    let mut no_evidence = 0;
    for row in rows.iter().filter(|r| counted(r).is_some()) {
        match row.env {
            Some(env) => ran[env as usize] += 1,
            None => no_evidence += 1,
        }
    }
    let _ = writeln!(w);
    let _ = writeln!(w, "## Where the evidence ran");
    let _ = writeln!(w);
    let _ = writeln!(w, "| Environment | Requirements |");
    let _ = writeln!(w, "|---|--:|");
    for (name, n) in [
        ("The development host", ran[Env::Host as usize]),
        ("An emulator (QEMU)", ran[Env::Emulation as usize]),
        (
            "A virtual machine on a real processor",
            ran[Env::Virtual as usize],
        ),
        ("Physical hardware", ran[Env::Hardware as usize]),
        ("No evidence yet", no_evidence),
    ] {
        let _ = writeln!(w, "| {name} | {} |", count(n));
    }

    // 5. By milestone. The lines are kept for the table of exact counts under "Details".
    let _ = writeln!(w);
    let _ = writeln!(w, "## By milestone");
    let _ = writeln!(w);
    let _ = writeln!(
        w,
        "Each milestone counts the requirements that [`docs/ROADMAP.md`](docs/ROADMAP.md) places in it. Its state is the roadmap's own word: done, built, partial or not started. That word is judged by the milestone's exit tests, not by the shares on its line. A milestone is done only when every one of its exit tests passes (decision items 255 and 256)."
    );
    let _ = writeln!(w);
    let _ = writeln!(w, "| Milestone | State in the roadmap | {STATES} |");
    let _ = writeln!(w, "|---|---|{STATES_RULE}|");
    let mut in_milestones: BTreeSet<usize> = BTreeSet::new();
    let mut milestones: Vec<(String, Tally)> = Vec::new();
    for (name, title) in &found.headings {
        let placed: Vec<&Row> = found
            .placed_in
            .iter()
            .filter(|(_, sections)| sections.len() == 1 && sections.contains(name))
            .map(|(&i, _)| &rows[i])
            .collect();
        in_milestones.extend(
            found
                .placed_in
                .iter()
                .filter(|(_, sections)| sections.contains(name))
                .map(|(&i, _)| i),
        );
        let state = states.get(name).map_or("-", String::as_str);
        let tally = Tally::of(placed);
        let _ = writeln!(w, "| {name}. {title} | {state} | {} |", tally.share_cells());
        milestones.push((format!("{name}. {title}"), tally));
    }
    let listed = Tally::of(
        found
            .placed_in
            .iter()
            .filter(|(i, _)| !in_milestones.contains(i))
            .map(|(&i, _)| &rows[i]),
    );
    let unplaced: Vec<&Row> = rows
        .iter()
        .enumerate()
        .filter(|(i, r)| counted(r).is_some() && !found.placed_in.contains_key(i))
        .map(|(_, r)| r)
        .collect();
    let unplaced_rules = unplaced.iter().filter(|r| r.kind == Kind::Rule).count();
    // What the roadmap does not place is its standing rules, and nothing else, whenever the
    // roadmap gate passes; if it ever were more, the page says how many are not rules.
    let others = unplaced.len() - unplaced_rules;
    let _ = writeln!(w);
    let _ = writeln!(
        w,
        "In no milestone: {} that the roadmap lists as in none, with its reasons, and {} standing rule{}, which belong to no milestone (decision item 256){}.",
        requirements_of(listed.rows()),
        count(unplaced_rules),
        if unplaced_rules == 1 { "" } else { "s" },
        if others == 0 {
            String::new()
        } else {
            format!("; and {} more that it does not place", count(others))
        }
    );

    // 6. By feature area.
    let _ = writeln!(w);
    let _ = writeln!(w, "## By feature area");
    let _ = writeln!(w);
    let _ = writeln!(
        w,
        "The requirements of `CLAUDE.md` that ask for something to be built, by the section they are in. Standing rules and ADR decisions are under \"Details\"."
    );
    let _ = writeln!(w);
    let build: Vec<&Row> = rows
        .iter()
        .filter(|r| r.kind == Kind::Build && r.file.starts_with("directive-"))
        .collect();
    let in_section = |section: u8| {
        build
            .iter()
            .copied()
            .filter(move |r| section_of(r) == Some(section))
    };
    let _ = writeln!(w, "| Feature area | Sections | {STATES} |");
    let _ = writeln!(w, "|---|---|{STATES_RULE}|");
    let mut grouped: BTreeSet<u8> = BTreeSet::new();
    let mut total = Tally::default();
    let mut areas: Vec<(String, Tally)> = Vec::new();
    for (name, first, last) in GROUPS {
        let tally = Tally::of((first..=last).flat_map(in_section));
        total.done += tally.done;
        total.built += tally.built;
        total.in_progress += tally.in_progress;
        total.not_started += tally.not_started;
        grouped.extend(first..=last);
        let _ = writeln!(
            w,
            "| {name} | §{first} to §{last} | {} |",
            tally.share_cells()
        );
        areas.push((format!("{name} | §{first} to §{last}"), tally));
    }
    let _ = writeln!(w, "| **All fourteen areas** | | {} |", total.share_cells());
    areas.push(("**All fourteen areas** |".to_string(), total));
    let mut outside: BTreeMap<(u8, String), usize> = BTreeMap::new();
    for row in build.iter().filter(|r| counted(r).is_some()) {
        if section_of(row).is_none_or(|section| !grouped.contains(&section)) {
            // In the directive's own order: its header first, its final rules last.
            let order =
                section_of(row).unwrap_or(if row.id.starts_with("F-") { u8::MAX } else { 0 });
            *outside.entry((order, place_of(row))).or_default() += 1;
        }
    }
    let outside_rows: usize = outside.values().sum();
    let places: Vec<String> = outside
        .iter()
        .map(|((_, place), n)| format!("{place} ({})", count(*n)))
        .collect();
    let _ = writeln!(w);
    let _ = writeln!(
        w,
        "Outside these areas: {} more of `CLAUDE.md` that ask for something to be built, in {}. They are counted in \"Overall\", and in a milestone where the roadmap places them.",
        count(outside_rows),
        if places.is_empty() {
            "no section".to_string()
        } else {
            places.join(", ")
        }
    );

    // 7. By specification (owner item 374; TASKS item 151).
    let _ = writeln!(w);
    let _ = writeln!(w, "## By specification");
    let _ = writeln!(w);
    let _ = writeln!(
        w,
        "Each specification, standard and protocol the project relies on, with the requirements that rest on it. The list is [`docs/requirements/specifications.md`](docs/requirements/specifications.md). The figures count Origin's requirements, not how much of a specification is implemented: a specification has many parts that no requirement asks for. No figure claims conformance, which needs the specification's own tests (CLAUDE.md §46). A specification with no requirement yet shows \"-\"."
    );
    let _ = writeln!(w);
    let mut by_spec: Vec<(String, Tally)> = Vec::new();
    if inputs.specs.is_empty() {
        let _ = writeln!(w, "No specification is listed.");
    } else {
        let _ = writeln!(w, "| Specification | {STATES} |");
        let _ = writeln!(w, "|---|{STATES_RULE}|");
        for spec in inputs.specs {
            let tally = Tally::of(specifications::rows_of(spec, rows));
            let title = format!("{} {}", spec.name, spec.revision);
            let _ = writeln!(w, "| {title} | {} |", tally.share_cells());
            by_spec.push((title, tally));
        }
    }

    // 8. Details, each closed until a reader opens it, so the page opens short.
    let _ = writeln!(w);
    let _ = writeln!(w, "## Details");
    let _ = writeln!(w);
    let _ = writeln!(w, "<details>");
    let _ = writeln!(
        w,
        "<summary>Each feature area, section by section</summary>"
    );
    for (name, first, last) in GROUPS {
        let _ = writeln!(w);
        let _ = writeln!(w, "**{name} (§{first} to §{last})**");
        let _ = writeln!(w);
        let _ = writeln!(w, "| Section | {STATES} |");
        let _ = writeln!(w, "|---|{STATES_RULE}|");
        for section in first..=last {
            let Some(title) = inputs.titles.get(&section) else {
                problems.push(format!(
                    "CLAUDE.md has no heading `## {section}. …`, which the feature area \"{name}\" needs"
                ));
                continue;
            };
            let _ = writeln!(
                w,
                "| §{section}. {title} | {} |",
                Tally::of(in_section(section)).share_cells()
            );
        }
    }
    let _ = writeln!(w);
    let _ = writeln!(w, "</details>");

    let _ = writeln!(w);
    let _ = writeln!(w, "<details>");
    let _ = writeln!(w, "<summary>ADR decisions</summary>");
    let _ = writeln!(w);
    let _ = writeln!(
        w,
        "The requirements of each accepted ADR, one for each of its decisions."
    );
    let _ = writeln!(w);
    let _ = writeln!(w, "| ADR | {STATES} |");
    let _ = writeln!(w, "|---|{STATES_RULE}|");
    let numbers: BTreeSet<u16> = rows.iter().filter_map(adr_of).collect();
    for number in numbers {
        let title = match inputs.adrs.get(&number) {
            Some((title, true)) if !title.is_empty() => title,
            _ => {
                problems.push(format!(
                    "the matrix holds rows of ADR-{number:04}, and docs/adr/ holds no accepted ADR-{number:04} with a title"
                ));
                continue;
            }
        };
        let tally = Tally::of(rows.iter().filter(|r| adr_of(r) == Some(number)));
        let _ = writeln!(w, "| ADR-{number:04}: {title} | {} |", tally.share_cells());
    }
    let _ = writeln!(w);
    let _ = writeln!(w, "</details>");

    let _ = writeln!(w);
    let _ = writeln!(w, "<details>");
    let _ = writeln!(w, "<summary>Standing rules</summary>");
    let _ = writeln!(w);
    let _ = writeln!(
        w,
        "Rules on how the work is done, of `CLAUDE.md` and of the ADRs. They hold at every milestone."
    );
    let _ = writeln!(w);
    let _ = writeln!(w, "| | {STATES} |");
    let _ = writeln!(w, "|---|{STATES_RULE}|");
    let _ = writeln!(
        w,
        "| Standing rules | {} |",
        Tally::of(rows.iter().filter(|r| r.kind == Kind::Rule)).share_cells()
    );
    let _ = writeln!(w);
    let _ = writeln!(w, "</details>");

    // Each specification's requirements, one block each, so a reader opens only the one wanted.
    for spec in inputs.specs {
        let resting = specifications::rows_of(spec, rows);
        let counted_rows: Vec<&Row> = resting
            .iter()
            .copied()
            .filter(|r| counted(r).is_some())
            .collect();
        {
            let _ = writeln!(w);
            let _ = writeln!(w, "<details>");
            let _ = writeln!(
                w,
                "<summary>{}. {} {}: {}</summary>",
                spec.id,
                spec.name,
                spec.revision,
                requirements_of(counted_rows.len())
            );
            let _ = writeln!(w);
            let baseline = if spec.baseline.is_empty() {
                "none".to_string()
            } else {
                spec.baseline.join(", ")
            };
            let crates = if spec.crates.is_empty() {
                "none yet".to_string()
            } else {
                spec.crates
                    .iter()
                    .map(|c| format!("`{c}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            let _ = writeln!(
                w,
                "Named in {}. §17 baseline: {baseline}. Implemented in: {crates}.",
                spec.named_in
            );
            let _ = writeln!(w);
            if counted_rows.is_empty() {
                let _ = writeln!(w, "No requirement rests on it yet.");
                let _ = writeln!(w);
                let _ = writeln!(w, "</details>");
                continue;
            }
            let _ = writeln!(w, "| Requirement | State |");
            let _ = writeln!(w, "|---|---|");
            for row in counted_rows {
                let state = match counted(row) {
                    Some(State::Done) => "done",
                    Some(State::Built) => "built",
                    Some(State::Partial | State::NotVerified) => "in progress",
                    _ => "not started",
                };
                let _ = writeln!(
                    w,
                    "| {}: {} | {state} |",
                    row.id,
                    row.requirement().replace('|', "\\|")
                );
            }
            let _ = writeln!(w);
            let _ = writeln!(w, "</details>");
        }
    }

    let _ = writeln!(w);
    let _ = writeln!(w, "<details>");
    let _ = writeln!(w, "<summary>Exact counts</summary>");
    let _ = writeln!(w);
    let _ = writeln!(
        w,
        "The milestone, feature-area and specification tables, with the number of requirements in each state in place of its share."
    );
    let _ = writeln!(w);
    let _ = writeln!(w, "| Milestone | {STATES} |");
    let _ = writeln!(w, "|---|{STATES_RULE}|");
    for (name, tally) in &milestones {
        let _ = writeln!(w, "| {name} | {} |", tally.count_cells());
    }
    let _ = writeln!(w);
    let _ = writeln!(w, "| Feature area | Sections | {STATES} |");
    let _ = writeln!(w, "|---|---|{STATES_RULE}|");
    for (name, tally) in &areas {
        let _ = writeln!(w, "| {name} | {} |", tally.count_cells());
    }
    if !by_spec.is_empty() {
        let _ = writeln!(w);
        let _ = writeln!(w, "| Specification | {STATES} |");
        let _ = writeln!(w, "|---|{STATES_RULE}|");
        for (name, tally) in &by_spec {
            let _ = writeln!(w, "| {name} | {} |", tally.count_cells());
        }
    }
    let _ = writeln!(w);
    let _ = writeln!(w, "</details>");

    // 9. How this page is made.
    let information = rows.iter().filter(|r| r.state.is_none()).count();
    let removed = rows
        .iter()
        .filter(|r| r.state == Some(State::Removed))
        .count();
    let _ = writeln!(w);
    let _ = writeln!(w, "## How this page is made");
    let _ = writeln!(w);
    let _ = writeln!(
        w,
        "- **From the records.** `{COMMAND}` writes it from four of them. They are the requirements matrix ([`docs/requirements/`](docs/requirements/README.md)), the headings of `CLAUDE.md` and of the ADRs, `docs/ROADMAP.md`, and the list of specifications beside the matrix. Every figure is a count of requirements or a share of one, and each requirement counts as one."
    );
    let _ = writeln!(
        w,
        "- **Not by hand.** `cargo xtask check` refuses a tree whose page differs from the one it generates, so the page on `main` is current after every merge."
    );
    let _ = writeln!(
        w,
        "- **Not counted:** {} kept for information only, and {} removed from scope. Only done counts as complete, and neither built nor in progress counts as coverage (decision items 193 and 221).",
        requirements_of(information),
        count(removed)
    );

    if problems.is_empty() {
        Ok(out)
    } else {
        Err(problems)
    }
}

/// The page this tree generates.
fn generate() -> Result<String> {
    let read = requirements::read()?;
    let rows = roadmap::matrix()?;
    let titles = requirements::section_titles(&read.directive);
    let adrs = requirements::adr_titles(&read.adrs);
    let roadmap = roadmap::read()?;
    let specs = specifications::read(&rows)?;
    page(&Inputs {
        rows: &rows,
        titles: &titles,
        adrs: &adrs,
        roadmap: &roadmap,
        specs: &specs,
    })
    .map_err(|problems| format!("{PAGE} cannot be generated:\n  {}", problems.join("\n  ")))
}

/// `cargo xtask progress`: writes the page.
pub(crate) fn write() -> Result {
    let page = generate()?;
    let path = repo_root().join(PAGE);
    fs::write(&path, &page).map_err(|e| format!("{}: {e}", path.display()))?;
    println!("xtask: progress: wrote {PAGE}");
    Ok(())
}

/// The first line on which `committed` and `generated` differ, from 1.
fn first_difference(committed: &str, generated: &str) -> usize {
    let mut a = committed.split_inclusive('\n');
    let mut b = generated.split_inclusive('\n');
    let mut line = 1;
    loop {
        match (a.next(), b.next()) {
            (Some(x), Some(y)) if x == y => line += 1,
            _ => return line,
        }
    }
}

/// Whether the committed page is the generated one, byte for byte.
fn compare(committed: Option<&str>, generated: &str) -> std::result::Result<(), String> {
    match committed {
        None => Err(format!(
            "{PAGE} is not in the tree: run `{COMMAND}` and commit it"
        )),
        Some(committed) if committed == generated => Ok(()),
        Some(committed) => Err(format!(
            "{PAGE} is not the page this tree generates (they differ from line {}): run `{COMMAND}` and commit it",
            first_difference(committed, generated)
        )),
    }
}

/// Refuses a tree that still holds a page at [`OLD_PAGE`] (the review of TASKS item 137,
/// R137-4). A branch cut before the move that regenerates the page writes it there, and a merge
/// that kept that file would leave a second page, in the old layout, that nothing regenerates and
/// no gate reads.
fn no_copy_at_the_old_place(present: bool) -> std::result::Result<(), String> {
    if present {
        return Err(format!(
            "{OLD_PAGE} is in the tree: the page is {PAGE}, at the repository root, since owner item 356. Remove {OLD_PAGE}; `{COMMAND}` writes the page at the root"
        ));
    }
    Ok(())
}

/// The gate: the committed page is the one the tree generates, byte for byte, and there is no
/// second page where it used to be.
pub(crate) fn gate() -> Result {
    let generated = generate()?;
    let committed = fs::read_to_string(repo_root().join(PAGE)).ok();
    compare(committed.as_deref(), &generated)?;
    no_copy_at_the_old_place(repo_root().join(OLD_PAGE).exists())?;
    println!(
        "xtask: progress gate: {PAGE} is the page this tree generates ({} lines)",
        generated.lines().count()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEADER: &str = "| ID | Requirement | Kind | Scope | Owner | State | Implementation | Acceptance | Evidence | Env | Review | Limitations | Decided by |\n|---|---|---|---|---|---|---|---|---|---|---|---|---|\n";

    /// A row of the fixture: the cells the page reads, and `-` or `none` for the rest.
    fn line(id: &str, kind: &str, scope: &str, state: &str, env: &str) -> String {
        format!(
            "| {id} | a statement | {kind} | {scope} | 2 | {state} | - | - | - | {env} | none | - | - |\n"
        )
    }

    /// Five small matrix files: the directive's header, §1, §8, its final rules, and ADR-0017.
    fn files() -> Vec<(String, String)> {
        let one = format!(
            "{HEADER}{}",
            line("S1-01", "build", "eng/host", "not-started", "-")
        );
        let eight = [
            line("S8-01", "build", "eng/host", "done", "host"),
            line("S8-02", "build", "both/hw", "built", "virt"),
            line("S8-03", "build", "both/hw", "partial", "host"),
            line("S8-04", "build", "both/hw", "not-verified", "-"),
            line("S8-05", "build", "both/hw", "not-started", "-"),
            line("S8-06", "rule", "eng/host", "done", "host"),
            line("S8-07", "info", "-", "-", "-"),
            line("S8-08", "build", "both/hw", "removed", "-"),
        ]
        .concat();
        let adr = [
            line("A17-Z1", "build", "both/hw", "built", "emu"),
            line("A17-Z2", "rule", "both/hw", "not-started", "-"),
        ]
        .concat();
        let unnumbered = |id: &str| {
            format!(
                "{HEADER}{}",
                line(id, "build", "eng/host", "not-started", "-")
            )
        };
        vec![
            ("adr-0017.md".into(), format!("{HEADER}{adr}")),
            ("directive-00-header.md".into(), unnumbered("H-01")),
            ("directive-01-authority.md".into(), one),
            (
                "directive-03-toolchain.md".into(),
                format!("{HEADER}{eight}"),
            ),
            ("directive-21-final-rules.md".into(), unnumbered("F-01")),
        ]
    }

    fn rows() -> Vec<Row> {
        let mut problems = Vec::new();
        let rows = requirements::matrix_rows(&files(), &mut problems);
        assert_eq!(problems, Vec::<String>::new());
        rows
    }

    /// A title for every numbered section the groups name.
    fn titles() -> BTreeMap<u8, String> {
        (1..=85).map(|n| (n, format!("Section {n}"))).collect()
    }

    fn adrs() -> BTreeMap<u16, (String, bool)> {
        BTreeMap::from([(17, ("Hardening".to_string(), true))])
    }

    const ROADMAP: &str = "# Roadmap\n\n## M0. First\n\n**Accepted.** **State, 2026-11-12: built, not done.**\n\n| Test | Passes when | Rows |\n|---|---|---|\n| M0-1 | it does | S8-01 to S8-03 |\n\n## M1. Second\n\n**Marked against the tests, 2026-11-12: not started.**\n\n| Rows | Subject |\n|---|---|\n| S8-04, S8-05, A17-Z1 | more |\n\n## Rows in no milestone\n\n| Rows | Why |\n|---|---|\n| S1-01, H-01, F-01 | a reason |\n";

    fn generated(
        roadmap: &str,
        titles: &BTreeMap<u8, String>,
    ) -> std::result::Result<String, Vec<String>> {
        page(&Inputs {
            rows: &rows(),
            titles,
            adrs: &adrs(),
            roadmap,
            specs: &[],
        })
    }

    #[test]
    fn the_figures_of_a_small_matrix() {
        let page = generated(ROADMAP, &titles()).expect("the fixture's page");
        for want in [
            // The one line, and overall: eleven counted requirements; S8-07 informs and S8-08 is
            // removed. Plain rounding would give 18.2% three times and 45.5%, which is 100.1%.
            "**18.2% of 11 requirements are done.** Done is the only state that counts as complete.",
            "| Done | 2 | 18.2% |",
            "| Built | 2 | 18.2% |",
            "| In progress | 2 | 18.2% |",
            "| Not started | 5 | 45.4% |",
            "| **Total** | **11** | **100.0%** |",
            // Where the evidence ran, one environment a line.
            "| The development host | 3 |",
            "| An emulator (QEMU) | 1 |",
            "| A virtual machine on a real processor | 1 |",
            "| Physical hardware | 0 |",
            "| No evidence yet | 6 |",
            // Milestones, with their state words in both of the roadmap's forms.
            "| M0. First | built | 3 | 33.3% | 33.3% | 33.4% | 0.0% |",
            "| M1. Second | not started | 3 | 0.0% | 33.3% | 33.3% | 33.4% |",
            "In no milestone: 3 requirements that the roadmap lists as in none, with its reasons, and 2 standing rules, which belong to no milestone (decision item 256).",
            // Feature areas: §8's build requirements; the rule and the removed one are not among
            // them. An area with no requirement shows "-" in each state's cell.
            "| Toolchain, build and CI | §8 to §15 | 5 | 20.0% | 20.0% | 40.0% | 20.0% |",
            "| Kernel core | §24 to §30 | 0 | - | - | - | - |",
            "| **All fourteen areas** | | 5 | 20.0% | 20.0% | 40.0% | 20.0% |",
            // The directive's unnumbered parts by name, in its own order: the header first, the
            // final rules last (the review's R137-5 (b)).
            "Outside these areas: 3 more of `CLAUDE.md` that ask for something to be built, in the header (1), §1 (1), the final rules (1). They are counted in \"Overall\", and in a milestone where the roadmap places them.",
            // The details: sections, ADRs (every kind), rules (of every file), exact counts.
            "**Toolchain, build and CI (§8 to §15)**",
            "| §8. Section 8 | 5 | 20.0% | 20.0% | 40.0% | 20.0% |",
            "| §9. Section 9 | 0 | - | - | - | - |",
            "| ADR-0017: Hardening | 2 | 0.0% | 50.0% | 0.0% | 50.0% |",
            "| Standing rules | 2 | 50.0% | 0.0% | 0.0% | 50.0% |",
            "| M0. First | 3 | 1 | 1 | 1 | 0 |",
            "| M1. Second | 3 | 0 | 1 | 1 | 1 |",
            "| Toolchain, build and CI | §8 to §15 | 5 | 1 | 1 | 2 | 1 |",
            "| Kernel core | §24 to §30 | 0 | 0 | 0 | 0 | 0 |",
            "| **All fourteen areas** | | 5 | 1 | 1 | 2 | 1 |",
            // What is not counted, with the two decision items.
            "- **Not counted:** 1 requirement kept for information only, and 1 removed from scope. Only done counts as complete, and neither built nor in progress counts as coverage (decision items 193 and 221).",
        ] {
            assert!(page.contains(want), "missing {want:?} in\n{page}");
        }
        assert_eq!(
            Some(page),
            generated(ROADMAP, &titles()).ok(),
            "one tree, one page"
        );
    }

    /// Owner item 357: the four percentages of a line add up to exactly 100.0. Every tally of up
    /// to 40 requirements:
    /// - the shares sum to 1,000 tenths, and each is within a tenth of its exact value;
    /// - a state with no requirement has no share;
    /// - done's share is the requirements gate's own figure, whatever the other three are (the
    ///   review's R137-1, as ruled);
    /// - no two states are shown against their counts, and equal counts differ by at most the
    ///   one tenth a tie gives away. (On a line of more than 500 requirements done and a state
    ///   one requirement away can be a tenth out of order, as the next test shows.)
    #[test]
    fn the_four_shares_of_a_line_sum_to_one_hundred() {
        let mut lines = 0;
        for done in 0..=40usize {
            for built in 0..=40 - done {
                for in_progress in 0..=40 - done - built {
                    for not_started in 0..=40 - done - built - in_progress {
                        let tally = Tally {
                            done,
                            built,
                            in_progress,
                            not_started,
                        };
                        let whole = tally.rows();
                        let Some(shares) = tally.shares() else {
                            assert_eq!(whole, 0);
                            continue;
                        };
                        lines += 1;
                        assert_eq!(shares.iter().sum::<usize>(), 1000, "{tally:?}");
                        for (part, share) in tally.parts().into_iter().zip(shares) {
                            // Within a tenth: share - 1 < exact < share + 1, in tenths.
                            assert!(share * whole < part * 1000 + whole, "{tally:?}");
                            assert!(part * 1000 < (share + 1) * whole, "{tally:?}");
                            if part == 0 {
                                assert_eq!(share, 0, "{tally:?}");
                            }
                        }
                        assert_eq!(
                            tenths(shares[0]),
                            requirements::percent(done, whole),
                            "{tally:?}"
                        );
                        // Among all four here: a line of up to 500 requirements cannot show
                        // two states out of order, since one requirement is two tenths.
                        for a in 0..4 {
                            for b in 0..4 {
                                if tally.parts()[a] == tally.parts()[b] {
                                    assert!(shares[a].abs_diff(shares[b]) <= 1, "{tally:?}");
                                }
                                if tally.parts()[a] < tally.parts()[b] {
                                    assert!(shares[a] <= shares[b], "{tally:?}");
                                }
                            }
                        }
                    }
                }
            }
        }
        assert!(lines > 100_000, "{lines}");
    }

    fn shares(done: usize, built: usize, in_progress: usize, not_started: usize) -> [usize; 4] {
        Tally {
            done,
            built,
            in_progress,
            not_started,
        }
        .shares()
        .expect("a line with a requirement")
    }

    /// The review's R137-1, as the coordinating session ruled it: done's share does not move
    /// when another state does, and it is the figure `cargo xtask check` prints.
    #[test]
    fn dones_share_is_its_own_whatever_the_others_are() {
        // The tree's own total on 2026-11-23, and the same with one requirement gone from not
        // started to built. Largest remainder over all four read 3.5% for the second, with done
        // still 58 of 1,686, which is 3.44%.
        assert_eq!(shares(58, 372, 248, 1008), [34, 221, 147, 598]);
        assert_eq!(shares(58, 373, 248, 1007), [34, 221, 147, 598]);
        assert_eq!(requirements::percent(58, 1686), "3.4%");
        // Two lines of that day's page that the first rule showed a tenth high.
        assert_eq!(shares(1, 9, 5, 54)[0], 14);
        assert_eq!(shares(17, 127, 82, 608)[0], 20);
        // Done's share over every split of the other 1,628 requirements among two states.
        for built in (0..=1628).step_by(37) {
            assert_eq!(shares(58, built, 0, 1628 - built)[0], 34, "{built}");
            assert_eq!(shares(58, 0, built, 1628 - built)[0], 34, "{built}");
        }
        // A half goes up, as the gate's does: 1 of 2,000 is 0.05%.
        assert_eq!(shares(1, 0, 0, 1999), [1, 0, 0, 999]);
        assert_eq!(requirements::percent(1, 2000), "0.1%");
        // The cost the ruling accepts: done and a state one requirement away from it, shown a
        // tenth out of order, either way. Done's own figure is the nearest tenth in both: it is
        // the other state's share that took an owed tenth, or was rounded down. Only a line of
        // more than 500 requirements can show it: below that, one requirement is two tenths.
        assert_eq!(shares(1241, 1240, 203, 384), [404, 405, 66, 125]);
        assert_eq!(shares(816, 817, 200, 1252), [265, 264, 65, 406]);
    }

    /// The other three share what done leaves, by largest remainder; a tie among them goes to
    /// the less complete state (the review's R137-5 (a): each line here fails for another order).
    #[test]
    fn a_tie_among_the_other_three_goes_to_the_less_complete() {
        assert_eq!(shares(1, 0, 0, 0), [1000, 0, 0, 0]);
        assert_eq!(shares(0, 1, 0, 0), [0, 1000, 0, 0]);
        assert_eq!(shares(1, 1, 1, 1), [250, 250, 250, 250]);
        // Thirds: done is 33.3%, to the nearest; of the other two, in progress takes the tenth.
        assert_eq!(shares(1, 1, 1, 0), [333, 333, 334, 0]);
        assert_eq!(shares(1, 1, 0, 1), [333, 333, 0, 334]);
        assert_eq!(shares(1, 0, 1, 1), [333, 0, 333, 334]);
        assert_eq!(shares(0, 1, 1, 1), [0, 333, 333, 334]);
        // Sixths: done is 16.7%, to the nearest; built and not started tie for the one tenth.
        assert_eq!(shares(1, 1, 0, 4), [167, 166, 0, 667]);
        assert_eq!(shares(1, 4, 0, 1), [167, 666, 0, 167]);
        assert_eq!(shares(1, 1, 4, 0), [167, 166, 667, 0]);
        // Two tenths owed among three equal remainders: the two less complete take them.
        assert_eq!(shares(3, 1, 1, 1), [500, 166, 167, 167]);
        // Not a tie: the larger remainder takes it, whichever state it is.
        assert_eq!(shares(0, 2, 0, 1), [0, 667, 0, 333]);
        assert_eq!(shares(0, 1, 0, 2), [0, 333, 0, 667]);
        // Ninths: 22.2% three times and 33.3% would be 99.9%.
        assert_eq!(shares(2, 2, 2, 3), [222, 222, 222, 334]);
        // §8 to §15 of the tree on 2026-11-23: plain rounding gave 20.8, 41.5, 5.7 and 32.1,
        // which is 100.1. Done keeps its 20.8.
        assert_eq!(shares(11, 22, 3, 17), [208, 415, 56, 321]);
    }

    #[test]
    fn a_thousand_or_more_is_written_with_commas() {
        for (n, want) in [
            (0, "0"),
            (7, "7"),
            (999, "999"),
            (1000, "1,000"),
            (1008, "1,008"),
            (12_345, "12,345"),
            (123_456, "123,456"),
            (1_234_567, "1,234,567"),
        ] {
            assert_eq!(count(n), want);
        }
        assert_eq!(requirements_of(1), "1 requirement");
        assert_eq!(requirements_of(0), "0 requirements");
        assert_eq!(requirements_of(1686), "1,686 requirements");
    }

    /// The lines of `page` that are headings, or that open or name a block of details.
    fn outline(page: &str) -> Vec<&str> {
        page.lines()
            .filter(|l| l.starts_with('#') || l.starts_with("<summary>"))
            .collect()
    }

    /// TASKS item 137: the page's sections, in the contract's order, and nothing else at their
    /// level; each block of details is closed, has a one-line summary, and has the blank lines
    /// Markdown needs to render what is inside it.
    #[test]
    fn the_sections_come_in_their_order_and_the_details_are_closed() {
        for page in [
            generated(ROADMAP, &titles()).expect("the fixture's page"),
            generate().expect("the page of this tree"),
        ] {
            // Each specification's block of details is checked below, with the others.
            let sections: Vec<&str> = outline(&page)
                .into_iter()
                .filter(|l| !l.starts_with("<summary>SP-"))
                .collect();
            assert_eq!(
                sections,
                [
                    "# Origin OS progress",
                    "## Overall",
                    "## What the states mean",
                    "## Where the evidence ran",
                    "## By milestone",
                    "## By feature area",
                    "## By specification",
                    "## Details",
                    "<summary>Each feature area, section by section</summary>",
                    "<summary>ADR decisions</summary>",
                    "<summary>Standing rules</summary>",
                    "<summary>Exact counts</summary>",
                    "## How this page is made",
                ]
            );
            let lines: Vec<&str> = page.lines().collect();
            assert!(lines[2].starts_with("**") && lines[2].contains(" are done.** "));
            let mut open = false;
            let mut blocks = 0;
            for (i, line) in lines.iter().enumerate() {
                match *line {
                    "<details>" => {
                        assert!(!open, "line {}", i + 1);
                        open = true;
                        blocks += 1;
                        assert!(lines[i + 1].starts_with("<summary>"), "line {}", i + 2);
                        assert!(lines[i + 1].ends_with("</summary>"), "line {}", i + 2);
                        assert_eq!(lines[i + 2], "", "line {}", i + 3);
                        assert_eq!(lines[i - 1], "", "line {}", i);
                    }
                    "</details>" => {
                        assert!(open, "line {}", i + 1);
                        open = false;
                        assert_eq!(lines[i - 1], "", "line {}", i);
                    }
                    _ => {
                        assert!(!line.contains("details>"), "line {}: {line}", i + 1);
                        assert!(
                            !line.contains("<summary>") || lines[i - 1] == "<details>",
                            "line {}",
                            i + 1
                        );
                    }
                }
            }
            assert!(!open);
            // The four fixed blocks, and one for each specification (TASKS item 151).
            let specifications = lines
                .iter()
                .filter(|l| l.starts_with("<summary>SP-"))
                .count();
            assert_eq!(blocks, 4 + specifications);
            // Nothing after the last block but how the page is made: three bullets.
            let made = lines
                .iter()
                .position(|l| *l == "## How this page is made")
                .expect("the last section");
            let after: Vec<&&str> = lines[made + 1..].iter().filter(|l| !l.is_empty()).collect();
            assert_eq!(after.len(), 3, "{after:?}");
            assert!(after.iter().all(|l| l.starts_with("- **")), "{after:?}");
        }
    }

    /// Owner item 357 and decision items 193 and 221: no figure and no sentence adds two states
    /// together. On both pages the only percentage outside a table is the first line's, which is
    /// done's alone, so no sentence can give a sum of two states (the review's R137-5 (d)); and
    /// the old column's and sentence's words are gone.
    #[test]
    fn nothing_adds_two_states_together() {
        for page in [
            generated(ROADMAP, &titles()).expect("the fixture's page"),
            generate().expect("the page of this tree"),
        ] {
            let lower = page.to_lowercase();
            for banned in [
                "done or built",
                "built or done",
                "done and built",
                "done plus built",
                "% done",
            ] {
                assert!(!lower.contains(banned), "the page says {banned:?}");
            }
            for (i, line) in page.lines().enumerate() {
                if line.contains('%') && !line.starts_with('|') {
                    assert_eq!(i, 2, "a percentage outside a table: {line}");
                    assert_eq!(line.matches('%').count(), 1, "{line}");
                    assert!(
                        line.contains("% of ") && line.contains(" are done.**"),
                        "{line}"
                    );
                }
            }
            // The headers of every table of states name the four, and only them, after the count.
            let mut tables = 0;
            for line in page
                .lines()
                .filter(|l| l.contains("| Requirements | Done |"))
            {
                tables += 1;
                assert!(
                    line.ends_with("| Requirements | Done | Built | In progress | Not started |"),
                    "{line}"
                );
            }
            assert!(tables >= 2 + 14 + 2 + 2, "{tables}");
        }
    }

    /// A reader sees "requirements" and "feature areas", not "rows" and "groups". Held on the
    /// fixture's page, whose titles are this test's own: the tree's page also carries the ADRs',
    /// the sections' and the milestones' titles, which the generator does not write (the review's
    /// R137-8).
    #[test]
    fn the_words_are_the_readers() {
        let page = generated(ROADMAP, &titles()).expect("the fixture's page");
        let lower = page.to_lowercase();
        for banned in [
            "feature group",
            "counted rows",
            "of that kind",
            "of every kind",
        ] {
            assert!(!lower.contains(banned), "the page says {banned:?}");
        }
        for word in lower.split(|c: char| !c.is_ascii_alphabetic()) {
            assert!(word != "row" && word != "rows", "the page says {word:?}");
            assert!(
                word != "group" && word != "groups",
                "the page says {word:?}"
            );
        }
    }

    /// The review's R137-5 (c): a thousand or more has its comma wherever the page writes a
    /// count. On the page this tree generates, which holds more than a thousand requirements, no
    /// cell after a line's first (the first is a title, which the generator does not write) and no
    /// sentence outside a table holds a run of four digits.
    #[test]
    fn every_count_on_the_trees_page_has_its_commas() {
        let four_digits = |text: &str| {
            text.as_bytes()
                .windows(4)
                .any(|run| run.iter().all(u8::is_ascii_digit))
        };
        let page = generate().expect("the page of this tree");
        let mut thousands = 0;
        // A specification's block quotes the list and the matrix, whose names, revisions and
        // requirements are titles, not counts (TASKS item 151); only its summary's count is the
        // generator's.
        let mut quoting = false;
        for line in page.lines() {
            if let Some(summary) = line.strip_prefix("<summary>SP-") {
                quoting = true;
                let count = summary.rsplit_once(": ").map_or("", |(_, count)| count);
                assert!(!four_digits(count), "a count without its comma: {line}");
                continue;
            }
            if quoting {
                quoting = line != "</details>";
                continue;
            }
            let written = match line.strip_prefix('|') {
                Some(cells) => cells.split_once('|').map_or("", |(_, rest)| rest),
                None => line,
            };
            assert!(!four_digits(written), "a count without its comma: {line}");
            thousands += written
                .as_bytes()
                .windows(5)
                .filter(|w| {
                    w[0].is_ascii_digit() && w[1] == b',' && w[2..].iter().all(u8::is_ascii_digit)
                })
                .count();
        }
        // The first line's total, the overall table's two, and the evidence table's.
        assert!(thousands >= 4, "{thousands}");
    }

    /// The four cells after a line's count, if the line is one of a table of percentages.
    fn percentages(line: &str) -> Option<Vec<usize>> {
        let cells: Vec<&str> = line.trim_matches('|').split('|').map(str::trim).collect();
        let last = cells.len().checked_sub(4).map(|at| &cells[at..])?;
        last.iter()
            .map(|cell| {
                let (whole, tenth) = cell.strip_suffix('%')?.split_once('.')?;
                Some(whole.parse::<usize>().ok()? * 10 + tenth.parse::<usize>().ok()?)
            })
            .collect()
    }

    /// Every line of percentages on the page this tree generates adds up to exactly 100.0, the
    /// line of the page's title gives the overall table's share of done, and the page has such
    /// lines for the milestones, the areas, their sections, the ADRs and the rules.
    #[test]
    fn every_line_of_the_trees_page_adds_up() {
        let page = generate().expect("the page of this tree");
        let mut lines = 0;
        for line in page.lines().filter(|l| l.starts_with('|')) {
            if let Some(shares) = percentages(line) {
                lines += 1;
                assert_eq!(shares.iter().sum::<usize>(), 1000, "{line}");
            }
        }
        // At least the milestones', the areas' and the rules' lines; the page has more.
        assert!(lines > 14 + 15 + 1, "{lines}");
        let done = page
            .lines()
            .find_map(|l| l.strip_prefix("| Done | "))
            .and_then(|rest| rest.split(" | ").nth(1))
            .map(|share| share.trim_end_matches(" |").to_string())
            .expect("the overall table's line for done");
        let title = page.lines().nth(2).expect("the one line");
        assert!(title.starts_with(&format!("**{done} of ")), "{title}");
    }

    /// The contract's "no sentence longer than two lines", taken as 200 characters of the
    /// page's own text: every sentence outside a table, on the page this tree generates.
    #[test]
    fn no_sentence_is_longer_than_two_lines() {
        let page = generate().expect("the page of this tree");
        for line in page.lines().filter(|l| !l.starts_with('|')) {
            for sentence in line.split(". ") {
                assert!(
                    sentence.chars().count() <= 200,
                    "{} characters: {sentence}",
                    sentence.chars().count()
                );
            }
        }
        for (_, meaning) in MEANINGS {
            assert!(meaning.chars().count() <= 200 && meaning.ends_with('.'));
            assert_eq!(meaning.matches(". ").count(), 0, "one sentence: {meaning}");
        }
    }

    /// Owner item 356: the page is at the root, so its links are written from the root. Each
    /// names a file or directory of this tree, and none climbs out or leaves the repository.
    #[test]
    fn the_links_are_written_from_the_root() {
        assert_eq!(PAGE, "PROGRESS.md");
        let page = generate().expect("the page of this tree");
        let mut links = 0;
        for (at, _) in page.match_indices("](") {
            let target = page[at + 2..].split(')').next().expect("a link's end");
            links += 1;
            assert!(
                !target.starts_with('/') && !target.starts_with('.'),
                "{target}"
            );
            assert!(
                !target.contains("://") && !target.contains(".."),
                "{target}"
            );
            let path = target.split('#').next().expect("a path");
            assert!(
                repo_root().join(path).exists(),
                "{target} is not in the tree"
            );
        }
        assert!(links >= 3, "{links}");
        // The files it names without a link are in the tree too.
        for named in [
            "CLAUDE.md",
            "docs/ROADMAP.md",
            "docs/requirements/README.md",
        ] {
            assert!(page.contains(named), "{named}");
            assert!(repo_root().join(named).exists(), "{named}");
        }
        assert!(!page.contains("docs/PROGRESS.md"));
    }

    #[test]
    fn a_milestone_without_one_state_is_refused() {
        let none = ROADMAP.replace("**State, 2026-11-12: built, not done.**", "");
        let problems = generated(&none, &titles()).expect_err("no state");
        assert!(
            problems.iter().any(|p| p.contains("M0 gives no state")),
            "{problems:?}"
        );
        let two = ROADMAP.replace(
            "| M0-1 | it does |",
            "**State, 2026-11-13: done.**\n\n| M0-1 | it does |",
        );
        let problems = generated(&two, &titles()).expect_err("two states");
        assert!(
            problems
                .iter()
                .any(|p| p.contains("M0 gives its state 2 times")),
            "{problems:?}"
        );
        let odd = ROADMAP.replace("built, not done", "nearly there");
        let problems = generated(&odd, &titles()).expect_err("an odd word");
        assert!(
            problems
                .iter()
                .any(|p| p.contains("M0's state is not one of")),
            "{problems:?}"
        );
    }

    #[test]
    fn a_section_with_no_heading_is_refused() {
        let mut titles = titles();
        titles.remove(&33);
        let problems = generated(ROADMAP, &titles).expect_err("no §33");
        assert!(
            problems
                .iter()
                .any(|p| p.contains("no heading `## 33. …`") && p.contains("\"Devices\"")),
            "{problems:?}"
        );
    }

    #[test]
    fn an_adr_with_no_title_is_refused() {
        for adrs in [
            BTreeMap::new(),
            BTreeMap::from([(17, ("Hardening".to_string(), false))]),
            BTreeMap::from([(17, (String::new(), true))]),
        ] {
            let problems = page(&Inputs {
                rows: &rows(),
                titles: &titles(),
                adrs: &adrs,
                roadmap: ROADMAP,
                specs: &[],
            })
            .expect_err("no accepted ADR-0017 with a title");
            assert!(
                problems.iter().any(|p| p.contains("ADR-0017")),
                "{problems:?}"
            );
        }
    }

    #[test]
    fn a_roadmap_the_roadmap_gate_refuses_is_refused() {
        let unplaced = ROADMAP.replace("S8-04, S8-05, A17-Z1", "S8-04, A17-Z1");
        let problems = generated(&unplaced, &titles()).expect_err("S8-05 is unplaced");
        assert!(
            problems.iter().any(|p| p.starts_with("S8-05 ")),
            "{problems:?}"
        );
    }

    #[test]
    fn nothing_counted_is_a_failure() {
        let problems = page(&Inputs {
            rows: &[],
            titles: &titles(),
            adrs: &adrs(),
            roadmap: ROADMAP,
            specs: &[],
        })
        .expect_err("no rows");
        assert!(
            problems.iter().any(|p| p.contains("no counted row")),
            "{problems:?}"
        );
    }

    #[test]
    fn a_stale_or_missing_page_is_refused() {
        let page = "# Progress\n\none\ntwo\n";
        assert_eq!(compare(Some(page), page), Ok(()));
        let stale = compare(Some("# Progress\n\none\nthree\n"), page).expect_err("stale");
        assert!(stale.contains("differ from line 4"), "{stale}");
        assert!(stale.contains("run `cargo xtask progress`"), "{stale}");
        let short = compare(Some("# Progress\n\none\n"), page).expect_err("short");
        assert!(short.contains("differ from line 4"), "{short}");
        let trailing = compare(Some("# Progress\n\none\ntwo"), page).expect_err("no newline");
        assert!(trailing.contains("differ from line 4"), "{trailing}");
        let missing = compare(None, page).expect_err("missing");
        assert!(missing.contains("is not in the tree"), "{missing}");
    }

    /// The review's R137-4: a page left where it used to be is refused, by name, and the tree
    /// holds none ([`the_tree_passes`]).
    #[test]
    fn a_copy_at_the_old_place_is_refused() {
        assert_eq!(no_copy_at_the_old_place(false), Ok(()));
        let refused = no_copy_at_the_old_place(true).expect_err("a second page");
        assert!(
            refused.starts_with("docs/PROGRESS.md is in the tree"),
            "{refused}"
        );
        assert!(
            refused.contains("the page is PROGRESS.md, at the repository root"),
            "{refused}"
        );
        assert!(refused.contains("Remove docs/PROGRESS.md"), "{refused}");
        assert!(!repo_root().join(OLD_PAGE).exists());
    }

    #[test]
    fn the_page_agrees_with_the_requirements_gate() {
        let rows = roadmap::matrix().expect("the matrix of this tree");
        let all = Tally::of(&rows);
        let figure = requirements::tree_counts();
        assert_eq!(
            (
                all.rows(),
                all.done,
                all.built,
                all.in_progress,
                all.not_started
            ),
            (figure.0, figure.1, figure.2, figure.3, figure.4)
        );
        let mut ran = [0usize; 4];
        for row in rows.iter().filter(|r| counted(r).is_some()) {
            if let Some(env) = row.env {
                ran[env as usize] += 1;
            }
        }
        assert_eq!(ran, figure.5);
        // The page's first figure is the gate's, for the same tree (the review's R137-1): the
        // title's line and the overall table's line for done.
        let gates = requirements::percent(figure.1, figure.0);
        let page = generate().expect("the page of this tree");
        let title = page.lines().nth(2).expect("the one line");
        assert!(
            title.starts_with(&format!(
                "**{gates} of {} are done.**",
                requirements_of(figure.0)
            )),
            "{title}"
        );
        assert!(
            page.contains(&format!("| Done | {} | {gates} |", count(figure.1))),
            "{gates}"
        );
    }

    #[test]
    fn the_tree_passes() {
        gate().expect("the progress page of this tree");
    }
}
