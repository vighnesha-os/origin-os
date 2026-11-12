//! `cargo xtask check`'s gate on the roadmap (CLAUDE.md §82; TASKS item 42; decision item 264).
//!
//! `docs/ROADMAP.md` lists each milestone's exit tests and the rows of the requirements matrix
//! (`docs/requirements/`) each milestone completes. Item 264 asks that every counted `build` row
//! of the matrix be placed in exactly one milestone, or listed as belonging to none with its
//! reason, and that a gate read the roadmap.
//!
//! **How a row is placed.** A table under a milestone's heading (`## M<n>. …`) names it in a
//! column headed "Rows"; a table under [`NONE_SECTION`] lists it as belonging to none. A cell of
//! that column holds, separated by commas, row IDs, ranges (`S8-01 to S8-09`, in the matrix's
//! order within one file) and prefixes (`A17-*`, every row whose ID starts `A17-`). Other words,
//! such as "the rows above", name nothing. A "Rows" column in any other section is read for its
//! IDs, which must exist, and places nothing.
//!
//! **What it refuses:**
//!
//! - a row ID, a range's end or a prefix the matrix does not hold, in any "Rows" column; a range
//!   that runs backwards or across two files; a piece of a cell that holds a row ID and is not one
//!   of the three forms;
//! - a counted `build` row (not `info`, not `removed`) placed in no milestone and not listed as
//!   belonging to none;
//! - a row placed in two milestones, or in a milestone and in the list of rows in none. A row may
//!   appear in several tables of one milestone;
//! - and, failing closed: a matrix the requirements gate's reader cannot read, no matrix row
//!   read, no milestone section, no placement read.
//!
//! **What it does not see:** whether a placement is right, whether a test's words match its rows,
//! and anything outside the "Rows" columns. Rows of kind `rule` need no place (item 256, rule 3).
//!
//! It reads the matrix through the requirements gate's reader ([`requirements::matrix_rows`]), and
//! the progress page (TASKS item 43) reads the roadmap through [`check`] and [`states`].

use std::collections::{BTreeMap, BTreeSet};
use std::fs;

use crate::provenance::cells;
use crate::requirements::{self, Kind, Row, State};
use crate::{Result, repo_root};

/// The page this gate reads.
const ROADMAP: &str = "docs/ROADMAP.md";
/// The matrix's directory.
const MATRIX: &str = "docs/requirements";
/// The heading of the section that lists rows belonging to no milestone.
const NONE_SECTION: &str = "## Rows in no milestone";
/// The column that places rows.
const ROWS: &str = "Rows";
/// What the list of rows in none is called in messages.
const NONE: &str = "\"Rows in no milestone\"";

impl Row {
    /// Whether item 264 asks for a place for it: a `build` row that is counted.
    fn needs_a_place(&self) -> bool {
        self.kind == Kind::Build && self.state != Some(State::Removed)
    }
}

/// Whether `text` has the shape of a row ID: a capital letter, digits, a dash, and letters or
/// digits (`S8-01`, `A05-C4`, `A01-def10`, `H-07`).
fn is_id(text: &str) -> bool {
    let Some((prefix, rest)) = text.split_once('-') else {
        return false;
    };
    let mut prefix = prefix.chars();
    prefix.next().is_some_and(|c| c.is_ascii_uppercase())
        && prefix.all(|c| c.is_ascii_digit())
        && !rest.is_empty()
        && rest.chars().all(|c| c.is_ascii_alphanumeric())
}

/// One piece of a "Rows" cell.
#[derive(Debug, PartialEq, Eq)]
enum Piece<'a> {
    Id(&'a str),
    Range(&'a str, &'a str),
    Prefix(&'a str),
}

/// The pieces of a "Rows" cell, or why one cannot be read. Words that hold no row ID are skipped.
fn pieces(cell: &str) -> std::result::Result<Vec<Piece<'_>>, String> {
    let mut out = Vec::new();
    for piece in cell.split(',') {
        let piece = piece.trim().trim_matches('`').trim();
        let piece = piece.strip_prefix("rows ").unwrap_or(piece).trim();
        if piece.is_empty() {
            continue;
        }
        if is_id(piece) {
            out.push(Piece::Id(piece));
        } else if let Some(prefix) = piece.strip_suffix("-*")
            && is_id(&format!("{prefix}-0"))
        {
            out.push(Piece::Prefix(prefix));
        } else if let Some((from, to)) = piece.split_once(" to ")
            && is_id(from.trim())
            && is_id(to.trim())
        {
            out.push(Piece::Range(from.trim(), to.trim()));
        } else if piece
            .split(|c: char| c.is_whitespace() || "()[];:.`".contains(c))
            .any(is_id)
        {
            return Err(format!(
                "\"{piece}\" holds a row ID but is not an ID, a range \"A to B\" or a prefix \"A17-*\""
            ));
        }
    }
    Ok(out)
}

/// Where a section places its rows.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Section {
    /// A milestone, by its name (`M3`).
    Milestone(String),
    /// The list of rows in no milestone.
    None,
    /// Anything else: its "Rows" columns are checked, and place nothing.
    Other,
}

/// The section a level-2 heading opens.
fn section(heading: &str) -> Section {
    if heading.trim_end() == NONE_SECTION {
        return Section::None;
    }
    let name = heading
        .strip_prefix("## M")
        .and_then(|rest| rest.split_once('.'))
        .filter(|(digits, _)| !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit()));
    match name {
        Some((digits, _)) => Section::Milestone(format!("M{digits}")),
        None => Section::Other,
    }
}

impl Section {
    fn name(&self) -> String {
        match self {
            Section::Milestone(name) => name.clone(),
            Section::None => NONE.to_string(),
            Section::Other => "another section".to_string(),
        }
    }
}

/// What the gate found: its problems, its counts for the report, and where each row is placed.
#[derive(Debug, Default)]
pub(crate) struct Found {
    pub(crate) problems: Vec<String>,
    milestones: usize,
    placed: usize,
    in_none: usize,
    needing: usize,
    /// Each milestone's name and title, as its heading `## M<n>. Title` gives them, in the
    /// roadmap's order.
    pub(crate) headings: Vec<(String, String)>,
    /// For each row placed, by its index in the rows given to [`check`], the sections it is
    /// placed in: milestones by name, or [`NONE`].
    pub(crate) placed_in: BTreeMap<usize, BTreeSet<String>>,
}

/// Reads `roadmap` against `rows` and returns what is wrong with it.
pub(crate) fn check(roadmap: &str, rows: &[Row]) -> Found {
    let mut found = Found::default();
    if rows.is_empty() {
        found
            .problems
            .push(format!("no row of the matrix was read from {MATRIX}/"));
        return found;
    }
    let position: BTreeMap<&str, usize> = rows
        .iter()
        .enumerate()
        .map(|(i, r)| (r.id.as_str(), i))
        .collect();
    let mut placed: BTreeMap<usize, BTreeSet<String>> = BTreeMap::new();
    let mut current = Section::Other;
    let mut milestones = BTreeSet::new();
    let mut column: Option<usize> = None;
    let mut fenced = false;
    for (index, line) in roadmap.lines().enumerate() {
        let at = format!("{ROADMAP}:{}", index + 1);
        if line.trim_start().starts_with("```") {
            fenced = !fenced;
            column = None;
            continue;
        }
        if fenced {
            continue;
        }
        if line.starts_with("## ") {
            current = section(line);
            if let Section::Milestone(name) = &current {
                milestones.insert(name.clone());
                let title = line.split_once(". ").map_or("", |(_, title)| title.trim());
                found.headings.push((name.clone(), title.to_string()));
            }
            column = None;
            continue;
        }
        let Some(cells) = cells(line) else {
            column = None;
            continue;
        };
        let Some(rows_at) = column else {
            column = Some(cells.iter().position(|c| c == ROWS).unwrap_or(usize::MAX));
            continue;
        };
        let rule = cells
            .iter()
            .all(|c| !c.is_empty() && c.chars().all(|ch| matches!(ch, '-' | ':')));
        let Some(cell) = cells.get(rows_at).filter(|_| !rule) else {
            continue;
        };
        let pieces = match pieces(cell) {
            Ok(pieces) => pieces,
            Err(problem) => {
                found.problems.push(format!("{at}: {problem}"));
                continue;
            }
        };
        let mut named: Vec<usize> = Vec::new();
        for piece in pieces {
            match piece {
                Piece::Id(id) => match position.get(id) {
                    Some(&i) => named.push(i),
                    None => found
                        .problems
                        .push(format!("{at}: the matrix holds no row {id}")),
                },
                Piece::Range(from, to) => match (position.get(from), position.get(to)) {
                    (Some(&a), Some(&b)) if a <= b && rows[a].file == rows[b].file => {
                        named.extend(a..=b);
                    }
                    (Some(_), Some(_)) => found.problems.push(format!(
                        "{at}: the range {from} to {to} runs backwards or across two files of the matrix"
                    )),
                    (a, b) => {
                        for (id, i) in [(from, a), (to, b)] {
                            if i.is_none() {
                                found.problems.push(format!(
                                    "{at}: the matrix holds no row {id}, an end of the range {from} to {to}"
                                ));
                            }
                        }
                    }
                },
                Piece::Prefix(prefix) => {
                    let start = format!("{prefix}-");
                    let before = named.len();
                    named.extend(
                        rows.iter()
                            .enumerate()
                            .filter(|(_, r)| r.id.starts_with(&start))
                            .map(|(i, _)| i),
                    );
                    if named.len() == before {
                        found
                            .problems
                            .push(format!("{at}: the matrix holds no row starting {start}"));
                    }
                }
            }
        }
        if current != Section::Other {
            for i in named {
                placed.entry(i).or_default().insert(current.name());
            }
        }
    }
    found.milestones = milestones.len();
    if milestones.is_empty() {
        found
            .problems
            .push(format!("{ROADMAP} has no milestone section (`## M<n>. …`)"));
    }
    if placed.is_empty() {
        found.problems.push(format!(
            "{ROADMAP} places no row: no table in a milestone's section has a \"{ROWS}\" column that names one"
        ));
    }
    for (&i, sections) in &placed {
        if sections.len() > 1 {
            let names: Vec<&str> = sections.iter().map(String::as_str).collect();
            found.problems.push(format!(
                "{} is placed in {}: a row belongs to one milestone, or to none",
                rows[i].id,
                names.join(" and ")
            ));
        }
        if sections.contains(NONE) {
            found.in_none += 1;
        } else {
            found.placed += 1;
        }
    }
    for (i, row) in rows.iter().enumerate() {
        if row.needs_a_place() {
            found.needing += 1;
            if !placed.contains_key(&i) {
                found.problems.push(format!(
                    "{} ({}, `build`, {}) is in no milestone and not in {NONE}",
                    row.id,
                    row.file,
                    row.state.map_or("-", State::named)
                ));
            }
        }
    }
    found.placed_in = placed;
    found
}

/// The words a milestone's state may start with, longest first so that `not started` is not
/// read as something shorter.
const STATE_WORDS: [&str; 4] = ["not started", "partial", "built", "done"];

/// What opens a sentence that gives a milestone's state: `**State, <date>: <word>` and
/// `**Marked against the tests, <date>: <word>`.
const STATE_OPENINGS: [&str; 2] = ["**State, ", "**Marked against the tests, "];

/// Each milestone's state word, by name, as its section of `roadmap` gives it, and what is wrong:
/// a milestone with no such sentence, with two, or with a word not in [`STATE_WORDS`].
pub(crate) fn states(roadmap: &str) -> (BTreeMap<String, String>, Vec<String>) {
    let mut found: BTreeMap<String, Vec<(usize, Option<&str>)>> = BTreeMap::new();
    let mut current: Option<String> = None;
    let mut fenced = false;
    for (index, line) in roadmap.lines().enumerate() {
        if line.trim_start().starts_with("```") {
            fenced = !fenced;
            continue;
        }
        if fenced {
            continue;
        }
        if line.starts_with("## ") {
            current = match section(line) {
                Section::Milestone(name) => {
                    found.entry(name.clone()).or_default();
                    Some(name)
                }
                _ => None,
            };
            continue;
        }
        let Some(name) = &current else {
            continue;
        };
        for opening in STATE_OPENINGS {
            for (at, _) in line.match_indices(opening) {
                let after = &line[at + opening.len()..];
                let word = after.split_once(": ").and_then(|(_, rest)| {
                    STATE_WORDS.into_iter().find(|word| rest.starts_with(word))
                });
                found
                    .entry(name.clone())
                    .or_default()
                    .push((index + 1, word));
            }
        }
    }
    let mut states = BTreeMap::new();
    let mut problems = Vec::new();
    for (name, sentences) in found {
        match sentences.as_slice() {
            [(_, Some(word))] => {
                states.insert(name, (*word).to_string());
            }
            [(line, None)] => problems.push(format!(
                "{ROADMAP}:{line}: {name}'s state is not one of {}",
                STATE_WORDS.join(", ")
            )),
            [] => problems.push(format!(
                "{ROADMAP}: {name} gives no state: a sentence starting `{}<date>: <word>`",
                STATE_OPENINGS[0]
            )),
            more => {
                let lines: Vec<String> = more.iter().map(|(line, _)| line.to_string()).collect();
                problems.push(format!(
                    "{ROADMAP}: {name} gives its state {} times, on lines {}: a milestone has one",
                    more.len(),
                    lines.join(", ")
                ));
            }
        }
    }
    (states, problems)
}

/// The roadmap as the checkout holds it.
pub(crate) fn read() -> Result<String> {
    let path = repo_root().join(ROADMAP);
    fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))
}

/// The matrix's rows, read by the requirements gate's reader; a matrix it cannot read is an error.
pub(crate) fn matrix() -> Result<Vec<Row>> {
    let mut problems = Vec::new();
    let rows = requirements::matrix_rows(&requirements::read()?.files, &mut problems);
    if problems.is_empty() {
        Ok(rows)
    } else {
        Err(format!(
            "the matrix under {MATRIX}/ cannot be read (`cargo xtask requirements-gate` says more):\n  {}",
            problems.join("\n  ")
        ))
    }
}

/// The gate: every counted `build` row of the matrix is placed in one milestone of the roadmap,
/// or listed as belonging to none, and every row the roadmap names exists.
pub(crate) fn gate() -> Result {
    let rows = matrix()?;
    let found = check(&read()?, &rows);
    if !found.problems.is_empty() {
        return Err(format!(
            "{ROADMAP} does not place every counted build row of the matrix once (CLAUDE.md §82, decision item 264; its rules are in developer-tools/xtask/src/roadmap.rs):\n  {}",
            found.problems.join("\n  ")
        ));
    }
    println!(
        "xtask: roadmap gate: {} counted build rows of {} in the matrix, each placed once; {} rows placed in {} milestones, {} listed in none",
        found.needing,
        rows.len(),
        found.placed,
        found.milestones,
        found.in_none
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEADER: &str = "| ID | Requirement | Kind | Scope | Owner | State | Implementation | Acceptance | Evidence | Env | Review | Limitations | Decided by |\n|---|---|---|---|---|---|---|---|---|---|---|---|---|\n";

    /// The cells after a row's state that this gate does not read.
    const REST: &str = "- | - | - | - | none | - | - |";

    /// Two small matrix files: `directive-01-a.md` with S1-01 to S1-05, `adr-0017.md` with
    /// A17-Z1 and A17-Z2.
    fn files() -> Vec<(String, String)> {
        let a = format!(
            "# PART\n\n{HEADER}| S1-01 | one | build | both/hw | 3 | built | {REST}\n| S1-02 | two | build | both/hw | 3 | not-started | {REST}\n| S1-03 | three | rule | eng/host | 28 | not-started | {REST}\n| S1-04 | four | info | - | 1 | - | {REST}\n| S1-05 | five | build | both/hw | 3 | removed | {REST}\n"
        );
        let b = format!(
            "{HEADER}| A17-Z1 | a | build | both/hw | 16 | partial | {REST}\n| A17-Z2 | b | build | both/hw | 16 | partial | {REST}\n"
        );
        vec![("directive-01-a.md".into(), a), ("adr-0017.md".into(), b)]
    }

    /// The fixture's rows, read by the requirements gate's reader.
    fn rows() -> Vec<Row> {
        let mut problems = Vec::new();
        let rows = requirements::matrix_rows(&files(), &mut problems);
        assert_eq!(problems, Vec::<String>::new());
        rows
    }

    /// A roadmap whose milestones place what `m0` and `m1` say, with `none` in no milestone.
    fn roadmap(m0: &str, m1: &str, none: &str) -> String {
        format!(
            "# Roadmap\n\n| Rows | Note |\n|---|---|\n| S1-01 | a citation outside any milestone places nothing |\n\n## M0. First\n\n| Test | Passes when | Rows |\n|---|---|---|\n| M0-1 | it does | {m0} |\n| M0-2 | it is shown | the rows above |\n\n## M1. Second\n\n| Rows | Subject |\n|---|---|\n| {m1} | more |\n\n## Rows in no milestone\n\n| Rows | Why |\n|---|---|\n| {none} | a reason |\n"
        )
    }

    fn problems(text: &str) -> Vec<String> {
        check(text, &rows()).problems
    }

    #[test]
    fn the_matrix_is_read_in_order() {
        let ids: Vec<String> = rows().into_iter().map(|r| r.id).collect();
        assert_eq!(
            ids,
            [
                "S1-01", "S1-02", "S1-03", "S1-04", "S1-05", "A17-Z1", "A17-Z2"
            ]
        );
        let needing: Vec<String> = rows()
            .into_iter()
            .filter(Row::needs_a_place)
            .map(|r| r.id)
            .collect();
        assert_eq!(needing, ["S1-01", "S1-02", "A17-Z1", "A17-Z2"]);
    }

    #[test]
    fn a_complete_roadmap_passes() {
        let found = check(&roadmap("S1-01 to S1-02", "rows A17-*", "S1-04"), &rows());
        assert_eq!(found.problems, Vec::<String>::new());
        assert_eq!((found.placed, found.in_none, found.needing), (4, 1, 4));
        assert_eq!(found.milestones, 2);
        let listed = roadmap(
            "S1-01, `S1-02`",
            "A17-Z1 to A17-Z2, the rows above",
            "S1-03",
        );
        assert_eq!(problems(&listed), Vec::<String>::new());
    }

    #[test]
    fn a_row_the_matrix_does_not_hold_is_refused() {
        for (m0, want) in [
            ("S1-01, S1-02, S1-09", "no row S1-09"),
            ("S1-01 to S1-09", "no row S1-09, an end of the range"),
            ("S1-01, S1-02, S2-*", "no row starting S2-"),
        ] {
            let found = problems(&roadmap(m0, "A17-*", "S1-04"));
            assert!(found.iter().any(|p| p.contains(want)), "{m0}: {found:?}");
        }
        let outside = roadmap("S1-01, S1-02", "A17-*", "S1-04").replace(
            "| S1-01 | a citation outside",
            "| S1-77 | a citation outside",
        );
        assert!(
            problems(&outside)
                .iter()
                .any(|p| p.contains("no row S1-77")),
            "an ID outside the milestones is checked too"
        );
    }

    #[test]
    fn a_build_row_in_no_milestone_is_refused() {
        let found = problems(&roadmap("S1-01", "A17-*", "S1-04"));
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].starts_with("S1-02 "), "{found:?}");
        assert!(found[0].contains("is in no milestone"), "{found:?}");
        // A rule, an info row and a removed row need no place.
        assert_eq!(
            problems(&roadmap("S1-01, S1-02", "A17-*", "S1-04")),
            Vec::<String>::new()
        );
        // Listing it as belonging to none is a place.
        assert_eq!(
            problems(&roadmap("S1-01", "A17-*", "S1-02")),
            Vec::<String>::new()
        );
    }

    #[test]
    fn a_row_placed_twice_is_refused() {
        let found = problems(&roadmap("S1-01, S1-02, A17-Z2", "A17-*", "S1-04"));
        assert!(
            found
                .iter()
                .any(|p| p.contains("A17-Z2 is placed in M0 and M1")),
            "{found:?}"
        );
        let found = problems(&roadmap("S1-01, S1-02", "A17-*", "S1-02"));
        assert!(
            found
                .iter()
                .any(|p| p.contains("S1-02 is placed in \"Rows in no milestone\" and M0")),
            "{found:?}"
        );
        // Twice within one milestone is one place.
        let twice = roadmap("S1-01, S1-02", "A17-*, A17-Z1", "S1-04");
        assert_eq!(problems(&twice), Vec::<String>::new());
    }

    #[test]
    fn a_cell_it_cannot_read_is_refused() {
        for m0 in [
            "S1-01 S1-02",
            "S1-02 to S1-01",
            "S1-02 to A17-Z1",
            "S1-01 to the end",
        ] {
            let found = problems(&roadmap(m0, "A17-*", "S1-04"));
            assert!(
                found
                    .iter()
                    .any(|p| p.contains("M0") || p.contains("ROADMAP") || p.contains(".md:")),
                "{m0}: {found:?}"
            );
            assert!(!found.is_empty(), "{m0}");
        }
        assert_eq!(pieces("the rows above"), Ok(vec![]));
        assert_eq!(pieces("`ONBF.md` §8.2, §13"), Ok(vec![]));
        assert_eq!(
            pieces("rows A17-*, S1-01 to S1-02, H-07"),
            Ok(vec![
                Piece::Prefix("A17"),
                Piece::Range("S1-01", "S1-02"),
                Piece::Id("H-07")
            ])
        );
    }

    #[test]
    fn nothing_read_is_a_failure() {
        assert!(
            !check(&roadmap("S1-01, S1-02", "A17-*", "S1-04"), &[])
                .problems
                .is_empty()
        );
        let no_rows_column = roadmap("S1-01, S1-02", "A17-*", "S1-04").replace("Rows", "Items");
        let found = problems(&no_rows_column);
        assert!(
            found.iter().any(|p| p.contains("places no row")),
            "{found:?}"
        );
        let no_milestone = "# Roadmap\n\n## Rows in no milestone\n\n| Rows | Why |\n|---|---|\n| S1-01 to S1-02, A17-* | all |\n";
        let found = problems(no_milestone);
        assert!(
            found.iter().any(|p| p.contains("no milestone section")),
            "{found:?}"
        );
    }

    #[test]
    fn ids_are_recognised() {
        for id in ["S8-01", "A05-C4", "A01-def10", "H-07", "F-41", "A14-Am1"] {
            assert!(is_id(id), "{id}");
        }
        for not in [
            "M0", "S8-", "-01", "s8-01", "S8-0.1", "§13", "A17-*", "S8 -01",
        ] {
            assert!(!is_id(not), "{not}");
        }
        assert_eq!(
            section("## M12. System updates"),
            Section::Milestone("M12".into())
        );
        assert_eq!(section("## Rows in no milestone"), Section::None);
        assert_eq!(section("## The Server edition"), Section::Other);
        assert_eq!(section("## Mx. Odd"), Section::Other);
    }

    #[test]
    fn the_tree_passes() {
        gate().expect("the roadmap of this tree");
    }
}
