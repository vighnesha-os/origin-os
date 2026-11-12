//! `cargo xtask progress`, which writes `docs/PROGRESS.md`, and the gate in `cargo xtask check`
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
//! state (no weighting), and two percentages, each labelled: done, and done or built. Nothing on
//! it depends on the clock, the host, a path or a commit hash, so one tree gives one page
//! (CLAUDE.md §75).
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

use crate::requirements::{self, Env, Kind, Row, State, percent};
use crate::roadmap;
use crate::{Result, repo_root};

/// The page this module writes and its gate reads.
const PAGE: &str = "docs/PROGRESS.md";
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

    /// The table cells: rows, the four counts, % done, % done or built.
    fn cells(self) -> String {
        let rows = self.rows();
        let share = |part: usize| {
            if rows == 0 {
                "-".to_string()
            } else {
                percent(part, rows)
            }
        };
        format!(
            "{} | {} | {} | {} | {} | {} | {}",
            rows,
            self.done,
            self.built,
            self.in_progress,
            self.not_started,
            share(self.done),
            share(self.done + self.built)
        )
    }
}

/// The header of every table of counts, after its first column.
const COUNTS: &str = "Rows | Done | Built | In progress | Not started | % done | % done or built";
/// Its rule, after the first column's.
const COUNTS_RULE: &str = "--:|--:|--:|--:|--:|--:|--:";

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

/// Where a directive row outside the numbered sections is: its ID's prefix (`H`, `P9`, `F`).
fn place_of(row: &Row) -> String {
    match section_of(row) {
        Some(section) => format!("§{section}"),
        None => row.id.split_once('-').map_or("?", |(p, _)| p).to_string(),
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
    let _ = writeln!(w, "# Progress");
    let _ = writeln!(w);
    let _ = writeln!(
        w,
        "Generated by `{COMMAND}` from the requirements matrix (`docs/requirements/`), the headings of `CLAUDE.md` and of the ADRs, and `docs/ROADMAP.md`. Do not edit it by hand: `cargo xtask check` fails on any pull request whose page differs from the one its tree generates, so the page on `main` is current after every merge. Every figure is a count of rows of the matrix, and every row counts as one, whatever its state."
    );
    let _ = writeln!(w);
    let _ = writeln!(
        w,
        "**What the states mean.** A requirement is one row of the matrix: one statement of `CLAUDE.md`, or one decision of an accepted ADR. **Done** means its acceptance criteria pass within its declared scope, on evidence from an environment at least as strong as that scope; only done counts toward the completion target (decision item 193). **Built** means it is implemented, with evidence, but short of its scope or of its acceptance criteria: for example, it passes under QEMU and its scope asks for a physical machine. **In progress** means part of it is implemented with evidence (`partial`), or something for it is in the tree with no evidence yet (`not-verified`). **Not started** means nothing for it is in the tree. Neither built nor in progress counts as coverage (decision items 193 and 221). Rows that only inform, and rows a decision removed from scope, are not counted."
    );

    // Overall.
    let _ = writeln!(w);
    let _ = writeln!(w, "## Overall");
    let _ = writeln!(w);
    let counted_rows = all.rows();
    let share = |part: usize| percent(part, counted_rows);
    let _ = writeln!(w, "| Counted requirements | Rows | Share |");
    let _ = writeln!(w, "|---|--:|--:|");
    for (name, part) in [
        ("Done", all.done),
        ("Built", all.built),
        ("In progress", all.in_progress),
        ("Not started", all.not_started),
    ] {
        let _ = writeln!(w, "| {name} | {part} | {} |", share(part));
    }
    let _ = writeln!(
        w,
        "| All counted | {counted_rows} | {} |",
        share(counted_rows)
    );
    let _ = writeln!(w);
    let _ = writeln!(
        w,
        "**Done: {}.** **Done or built: {}.**",
        share(all.done),
        share(all.done + all.built)
    );
    let information = rows.iter().filter(|r| r.state.is_none()).count();
    let removed = rows
        .iter()
        .filter(|r| r.state == Some(State::Removed))
        .count();
    let _ = writeln!(w);
    let _ = writeln!(
        w,
        "Not counted: {information} rows that only inform and {removed} removed from scope, of {} rows in all.",
        rows.len()
    );
    let mut ran = [0usize; 4];
    let mut no_evidence = 0;
    for row in rows.iter().filter(|r| counted(r).is_some()) {
        match row.env {
            Some(env) => ran[env as usize] += 1,
            None => no_evidence += 1,
        }
    }
    let _ = writeln!(w);
    let _ = writeln!(w, "Where the counted rows' evidence ran:");
    let _ = writeln!(w);
    let _ = writeln!(
        w,
        "| On the development host | Under emulation (QEMU) | In a virtual machine on a real processor | On physical hardware | No evidence |"
    );
    let _ = writeln!(w, "|--:|--:|--:|--:|--:|");
    let _ = writeln!(
        w,
        "| {} | {} | {} | {} | {} |",
        ran[Env::Host as usize],
        ran[Env::Emulation as usize],
        ran[Env::Virtual as usize],
        ran[Env::Hardware as usize],
        no_evidence
    );

    // By milestone.
    let _ = writeln!(w);
    let _ = writeln!(w, "## By milestone");
    let _ = writeln!(w);
    let _ = writeln!(
        w,
        "The counted rows `docs/ROADMAP.md` places in each milestone: its exit tests and the further rows placed there. The state is the milestone's own, as the roadmap gives it; a milestone is done only when every one of its accepted exit tests passes in its declared scope (decision items 255 and 256)."
    );
    let _ = writeln!(w);
    let _ = writeln!(w, "| Milestone | State in the roadmap | {COUNTS} |");
    let _ = writeln!(w, "|---|---|{COUNTS_RULE}|");
    let mut in_milestones: BTreeSet<usize> = BTreeSet::new();
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
        let _ = writeln!(
            w,
            "| {name}. {title} | {state} | {} |",
            Tally::of(placed).cells()
        );
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
        "In no milestone: {} counted rows the roadmap lists under \"Rows in no milestone\", with its reasons, and {} counted rows it does not place ({unplaced_rules} of them standing rules, which belong to no milestone by decision item 256).",
        listed.rows(),
        unplaced.len()
    );

    // By feature group.
    let _ = writeln!(w);
    let _ = writeln!(w, "## By feature group");
    let _ = writeln!(w);
    let _ = writeln!(
        w,
        "The counted `build` rows of `CLAUDE.md`, grouped by the section they are in. Standing rules are counted below, and the ADRs' rows by ADR."
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
    let _ = writeln!(w, "| Group | Sections | {COUNTS} |");
    let _ = writeln!(w, "|---|---|{COUNTS_RULE}|");
    let mut grouped: BTreeSet<u8> = BTreeSet::new();
    let mut total = Tally::default();
    for (name, first, last) in GROUPS {
        let tally = Tally::of((first..=last).flat_map(in_section));
        total.done += tally.done;
        total.built += tally.built;
        total.in_progress += tally.in_progress;
        total.not_started += tally.not_started;
        grouped.extend(first..=last);
        let _ = writeln!(w, "| {name} | §{first} to §{last} | {} |", tally.cells());
    }
    let _ = writeln!(w, "| All fourteen groups | | {} |", total.cells());
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
        "Outside these groups: {outside_rows} counted `build` rows of `CLAUDE.md`, in {}. They are counted in \"Overall\" and in the milestones.",
        if places.is_empty() {
            "no section".to_string()
        } else {
            places.join(", ")
        }
    );
    for (name, first, last) in GROUPS {
        let _ = writeln!(w);
        let _ = writeln!(w, "### {name} (§{first} to §{last})");
        let _ = writeln!(w);
        let _ = writeln!(w, "| Section | {COUNTS} |");
        let _ = writeln!(w, "|---|{COUNTS_RULE}|");
        for section in first..=last {
            let Some(title) = inputs.titles.get(&section) else {
                problems.push(format!(
                    "CLAUDE.md has no heading `## {section}. …`, which the group \"{name}\" needs"
                ));
                continue;
            };
            let _ = writeln!(
                w,
                "| §{section}. {title} | {} |",
                Tally::of(in_section(section)).cells()
            );
        }
    }

    // ADR decisions.
    let _ = writeln!(w);
    let _ = writeln!(w, "## ADR decisions");
    let _ = writeln!(w);
    let _ = writeln!(
        w,
        "The counted rows of each accepted ADR, one row for each of its decisions, of every kind."
    );
    let _ = writeln!(w);
    let _ = writeln!(w, "| ADR | {COUNTS} |");
    let _ = writeln!(w, "|---|{COUNTS_RULE}|");
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
        let _ = writeln!(w, "| ADR-{number:04}: {title} | {} |", tally.cells());
    }

    // Standing rules.
    let _ = writeln!(w);
    let _ = writeln!(w, "## Standing rules");
    let _ = writeln!(w);
    let _ = writeln!(
        w,
        "The counted rows of kind `rule`, of `CLAUDE.md` and of the ADRs: rules on how the work is done, which hold at every milestone."
    );
    let _ = writeln!(w);
    let _ = writeln!(w, "| | {COUNTS} |");
    let _ = writeln!(w, "|---|{COUNTS_RULE}|");
    let _ = writeln!(
        w,
        "| Standing rules | {} |",
        Tally::of(rows.iter().filter(|r| r.kind == Kind::Rule)).cells()
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
    fn the_counts_of_a_small_matrix() {
        let page = generated(ROADMAP, &titles()).expect("the fixture's page");
        for want in [
            // Overall: nine counted rows; S8-07 informs and S8-08 is removed.
            "| Done | 2 | 22.2% |",
            "| Built | 2 | 22.2% |",
            "| In progress | 2 | 22.2% |",
            "| Not started | 3 | 33.3% |",
            "| All counted | 9 | 100.0% |",
            "**Done: 22.2%.** **Done or built: 44.4%.**",
            "Not counted: 1 rows that only inform and 1 removed from scope, of 11 rows in all.",
            "| 3 | 1 | 1 | 0 | 4 |",
            // Milestones, with their state words in both of the roadmap's forms.
            "| M0. First | built | 3 | 1 | 1 | 1 | 0 | 33.3% | 66.7% |",
            "| M1. Second | not started | 3 | 0 | 1 | 1 | 1 | 0.0% | 33.3% |",
            "In no milestone: 1 counted rows the roadmap lists under \"Rows in no milestone\", with its reasons, and 2 counted rows it does not place (2 of them standing rules",
            // Groups: §8's build rows; the rule and the removed row are not among them.
            "| Toolchain, build and CI | §8 to §15 | 5 | 1 | 1 | 2 | 1 | 20.0% | 40.0% |",
            "| Kernel core | §24 to §30 | 0 | 0 | 0 | 0 | 0 | - | - |",
            "| All fourteen groups | | 5 | 1 | 1 | 2 | 1 | 20.0% | 40.0% |",
            "Outside these groups: 1 counted `build` rows of `CLAUDE.md`, in §1 (1).",
            "### Toolchain, build and CI (§8 to §15)",
            "| §8. Section 8 | 5 | 1 | 1 | 2 | 1 | 20.0% | 40.0% |",
            "| §9. Section 9 | 0 | 0 | 0 | 0 | 0 | - | - |",
            // ADRs: every kind; rules: of every file.
            "| ADR-0017: Hardening | 2 | 0 | 1 | 0 | 1 | 0.0% | 50.0% |",
            "| Standing rules | 2 | 1 | 0 | 0 | 1 | 50.0% | 50.0% |",
        ] {
            assert!(page.contains(want), "missing {want:?} in\n{page}");
        }
        assert_eq!(
            Some(page),
            generated(ROADMAP, &titles()).ok(),
            "one tree, one page"
        );
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
