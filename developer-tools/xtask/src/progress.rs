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
//! items 193 and 221). Nothing on it depends on the clock, the host, a path or a commit hash, so
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
use crate::{Result, repo_root};

/// The page this module writes and its gate reads.
const PAGE: &str = "PROGRESS.md";
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
/// item 357; the coordinating session's contract for TASKS item 137 gives the sentences).
const MEANINGS: [(&str, &str); 4] = [
    (
        "Done",
        "Its acceptance tests pass in the environment the requirement asks for.",
    ),
    (
        "Built",
        "It is written and tested, but in a weaker environment than it asks for, usually an emulator where it asks for a real machine.",
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
    /// Largest remainder: each share is rounded down, and the tenths still owed go to the
    /// states with the largest remainders, one each. Where two remainders are equal, the less
    /// complete state takes the tenth, so that done is never rounded up on a tie. Every share is
    /// within a tenth of its exact value.
    fn shares(self) -> Option<[usize; 4]> {
        let whole = self.rows();
        if whole == 0 {
            return None;
        }
        let parts = self.parts();
        let mut shares = parts.map(|part| part * 1000 / whole);
        let owed = 1000 - shares.iter().sum::<usize>();
        // The states by remainder, largest first; among equals, the less complete first.
        let mut order = [3, 2, 1, 0];
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
        "Each milestone counts the requirements that [`docs/ROADMAP.md`](docs/ROADMAP.md) places in it. The state is the milestone's own, as the roadmap gives it. A milestone is done only when every one of its exit tests passes (decision items 255 and 256)."
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
    let _ = writeln!(w);
    let _ = writeln!(
        w,
        "In no milestone: {} that the roadmap lists as in none, with its reasons, and {} that it does not place. Of those it does not place, {} are standing rules, which belong to no milestone (decision item 256).",
        requirements_of(listed.rows()),
        count(unplaced.len()),
        count(unplaced_rules)
    );

    // 6. By feature area.
    let _ = writeln!(w);
    let _ = writeln!(w, "## By feature area");
    let _ = writeln!(w);
    let _ = writeln!(
        w,
        "The requirements of [`CLAUDE.md`](CLAUDE.md) that ask for something to be built, by the section they are in. Standing rules and ADR decisions are under \"Details\"."
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
            let order = section_of(row).unwrap_or(0);
            *outside.entry((order, place_of(row))).or_default() += 1;
        }
    }
    let outside_rows: usize = outside.values().sum();
    let places: Vec<String> = outside
        .iter()
        .map(|((_, place), n)| format!("{place} ({n})"))
        .collect();
    let _ = writeln!(w);
    let _ = writeln!(
        w,
        "Outside these areas: {} of that kind, in {}. They are counted in \"Overall\" and in the milestones.",
        requirements_of(outside_rows),
        if places.is_empty() {
            "no section".to_string()
        } else {
            places.join(", ")
        }
    );

    // 7. Details, each closed until a reader opens it, so the page opens short.
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
        "The requirements of each accepted ADR: one for each of its decisions, of every kind."
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

    let _ = writeln!(w);
    let _ = writeln!(w, "<details>");
    let _ = writeln!(w, "<summary>Exact counts</summary>");
    let _ = writeln!(w);
    let _ = writeln!(
        w,
        "The two tables above, with the number of requirements in each state in place of its share."
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
    let _ = writeln!(w);
    let _ = writeln!(w, "</details>");

    // 8. How this page is made.
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
        "- **From the records.** `{COMMAND}` writes it from three of them. They are the requirements matrix ([`docs/requirements/`](docs/requirements/README.md)), the headings of `CLAUDE.md` and of the ADRs, and `docs/ROADMAP.md`. Every figure is a count of requirements, and each counts as one."
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
    page(&Inputs {
        rows: &rows,
        titles: &titles,
        adrs: &adrs,
        roadmap: &roadmap,
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

/// The gate: the committed page is the one the tree generates, byte for byte.
pub(crate) fn gate() -> Result {
    let generated = generate()?;
    let committed = fs::read_to_string(repo_root().join(PAGE)).ok();
    compare(committed.as_deref(), &generated)?;
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

    /// Three small matrix files: §1, §8 and ADR-0017.
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
        vec![
            ("adr-0017.md".into(), format!("{HEADER}{adr}")),
            ("directive-01-authority.md".into(), one),
            (
                "directive-03-toolchain.md".into(),
                format!("{HEADER}{eight}"),
            ),
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

    const ROADMAP: &str = "# Roadmap\n\n## M0. First\n\n**Accepted.** **State, 2026-11-12: built, not done.**\n\n| Test | Passes when | Rows |\n|---|---|---|\n| M0-1 | it does | S8-01 to S8-03 |\n\n## M1. Second\n\n**Marked against the tests, 2026-11-12: not started.**\n\n| Rows | Subject |\n|---|---|\n| S8-04, S8-05, A17-Z1 | more |\n\n## Rows in no milestone\n\n| Rows | Why |\n|---|---|\n| S1-01 | a reason |\n";

    fn generated(
        roadmap: &str,
        titles: &BTreeMap<u8, String>,
    ) -> std::result::Result<String, Vec<String>> {
        page(&Inputs {
            rows: &rows(),
            titles,
            adrs: &adrs(),
            roadmap,
        })
    }

    #[test]
    fn the_figures_of_a_small_matrix() {
        let page = generated(ROADMAP, &titles()).expect("the fixture's page");
        for want in [
            // The one line, and overall: nine counted requirements; S8-07 informs and S8-08 is
            // removed. Two ninths are 22.2% three times and three ninths 33.3%, which is 99.9%:
            // the tenth still owed goes to the largest remainder, not started's.
            "**22.2% of 9 requirements are done.** Done is the only state that counts as complete.",
            "| Done | 2 | 22.2% |",
            "| Built | 2 | 22.2% |",
            "| In progress | 2 | 22.2% |",
            "| Not started | 3 | 33.4% |",
            "| **Total** | **9** | **100.0%** |",
            // Where the evidence ran, one environment a line.
            "| The development host | 3 |",
            "| An emulator (QEMU) | 1 |",
            "| A virtual machine on a real processor | 1 |",
            "| Physical hardware | 0 |",
            "| No evidence yet | 4 |",
            // Milestones, with their state words in both of the roadmap's forms.
            "| M0. First | built | 3 | 33.3% | 33.3% | 33.4% | 0.0% |",
            "| M1. Second | not started | 3 | 0.0% | 33.3% | 33.3% | 33.4% |",
            "In no milestone: 1 requirement that the roadmap lists as in none, with its reasons, and 2 that it does not place. Of those it does not place, 2 are standing rules",
            // Feature areas: §8's build requirements; the rule and the removed one are not among
            // them. An area with no requirement shows "-" in each state's cell.
            "| Toolchain, build and CI | §8 to §15 | 5 | 20.0% | 20.0% | 40.0% | 20.0% |",
            "| Kernel core | §24 to §30 | 0 | - | - | - | - |",
            "| **All fourteen areas** | | 5 | 20.0% | 20.0% | 40.0% | 20.0% |",
            "Outside these areas: 1 requirement of that kind, in §1 (1).",
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
    /// to 40 requirements: the shares sum to 1,000 tenths, each is within a tenth of its exact
    /// value, a state with no requirement has no share, and equal counts differ by at most the
    /// one tenth a tie gives away.
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
                            assert_eq!(part == 0, share == 0, "{tally:?}");
                        }
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
        let shares = |done, built, in_progress, not_started| {
            Tally {
                done,
                built,
                in_progress,
                not_started,
            }
            .shares()
        };
        assert_eq!(shares(0, 0, 0, 0), None);
        assert_eq!(shares(1, 0, 0, 0), Some([1000, 0, 0, 0]));
        // A tie goes to the less complete state: done is never rounded up on one.
        assert_eq!(shares(1, 1, 1, 0), Some([333, 333, 334, 0]));
        assert_eq!(shares(1, 1, 0, 1), Some([333, 333, 0, 334]));
        assert_eq!(shares(1, 0, 0, 2), Some([333, 0, 0, 667]));
        assert_eq!(shares(2, 0, 0, 1), Some([667, 0, 0, 333]));
        assert_eq!(shares(1, 1, 1, 1), Some([250, 250, 250, 250]));
        // §8 to §15 of the tree on 2026-11-23: plain rounding gave 20.8, 41.5, 5.7 and 32.1,
        // which is 100.1.
        assert_eq!(shares(11, 22, 3, 17), Some([207, 415, 57, 321]));
        // The tree's own total that day.
        assert_eq!(shares(58, 372, 248, 1008), Some([34, 221, 147, 598]));
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
            assert_eq!(
                outline(&page),
                [
                    "# Origin OS progress",
                    "## Overall",
                    "## What the states mean",
                    "## Where the evidence ran",
                    "## By milestone",
                    "## By feature area",
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
            assert_eq!(blocks, 4);
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
    /// together, and a reader sees "requirements" and "feature areas", not "rows" and "groups".
    #[test]
    fn nothing_adds_two_states_together_and_the_words_are_the_readers() {
        for page in [
            generated(ROADMAP, &titles()).expect("the fixture's page"),
            generate().expect("the page of this tree"),
        ] {
            let lower = page.to_lowercase();
            for banned in [
                "done or built",
                "built or done",
                "done and built",
                "% done",
                "feature group",
                "counted rows",
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
            // The headers of every table of states name the four, and only them, after the count.
            for line in page
                .lines()
                .filter(|l| l.contains("| Requirements | Done |"))
            {
                assert!(
                    line.ends_with("| Requirements | Done | Built | In progress | Not started |"),
                    "{line}"
                );
            }
        }
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
        assert!(lines > 14 + 14 + 59 + 1, "{lines}");
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
    }

    #[test]
    fn the_tree_passes() {
        gate().expect("the progress page of this tree");
    }
}
