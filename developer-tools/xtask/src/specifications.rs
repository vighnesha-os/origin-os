//! The list of specifications, standards and protocols the project relies on, which the progress
//! page shows with the requirements that rest on each (owner item 374; TASKS item 151).
//!
//! The list is `docs/requirements/specifications.md`: one table, one row per specification. A
//! row names the matrix rows that rest on the specification, either by ID or as a prefix ending
//! in `*` (`S33-*`, `A19-*`), so a row added later to that section or ADR is counted with no edit
//! here. It also names the §17 baseline statements the specification stands for, and the crates
//! that implement parts of it.
//!
//! **What it refuses, failing closed, through the progress page's generation:**
//! - a file with no such table, or with two;
//! - a row that is not eight cells;
//! - an ID that is not `SP-NN`, or that appears twice;
//! - a kind outside [`KINDS`];
//! - a matrix ID that is not a row, and a prefix that matches no row;
//! - a baseline cell naming a row that is not one of §17's baseline statements;
//! - a baseline statement of §17 that no specification names;
//! - a crate that the provenance table has no row for.
//!
//! **What it does not see:** whether a row truly rests on the specification, or whether a
//! specification the project relies on is missing from the list, unless §17 names it. Both are
//! judgements, made by whoever edits the list and read in review.

use std::collections::BTreeSet;
use std::fs;

use crate::provenance;
use crate::requirements::Row;
use crate::{Result, repo_root};

/// The list's name in the matrix's directory. The matrix's reader leaves it out
/// (`requirements::read`), since it holds no rows of the matrix.
pub(crate) const FILE_NAME: &str = "specifications.md";

/// The list's path from the repository's root.
pub(crate) const FILE: &str = "docs/requirements/specifications.md";

/// The provenance table, whose rows name the crates.
const LEDGER: &str = "docs/engineering/SOURCE_PROVENANCE.md";

/// The list's columns, in order.
const COLUMNS: [&str; 8] = [
    "ID",
    "Specification",
    "Revision",
    "Kind",
    "Named in",
    "Baseline",
    "Rows",
    "Crates",
];

/// What a specification is: a published standard, a vendor's document, Origin's own
/// specification, or the toolchain.
const KINDS: [&str; 4] = ["standard", "vendor", "origin", "toolchain"];

/// The words that open each of §17's baseline statements, as the matrix quotes them.
const BASELINE: &str = "Initial reference baseline:";

/// One row of the list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Spec {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) revision: String,
    pub(crate) kind: String,
    pub(crate) named_in: String,
    /// §17's baseline statements it stands for, by matrix ID.
    pub(crate) baseline: Vec<String>,
    /// The matrix rows that rest on it: IDs, or prefixes ending in `*`.
    pub(crate) selectors: Vec<String>,
    pub(crate) crates: Vec<String>,
}

/// A cell's entries: `-` for none, or entries separated by `, `.
fn entries(cell: &str) -> Vec<String> {
    if cell == "-" {
        return Vec::new();
    }
    cell.split(", ")
        .map(str::trim)
        .filter(|e| !e.is_empty())
        .map(str::to_string)
        .collect()
}

/// The list's rows, or what keeps them from being read.
pub(crate) fn parse(text: &str) -> std::result::Result<Vec<Spec>, Vec<String>> {
    let header = format!("| {} |", COLUMNS.join(" | "));
    let mut problems = Vec::new();
    let mut specs = Vec::new();
    let mut tables = 0;
    let mut in_table = false;
    for (index, line) in text.lines().enumerate() {
        let at = format!("{FILE}:{}", index + 1);
        let trimmed = line.trim();
        if trimmed == header {
            tables += 1;
            in_table = true;
            continue;
        }
        if !in_table {
            continue;
        }
        if !trimmed.starts_with('|') {
            in_table = false;
            continue;
        }
        if trimmed.starts_with("|---") {
            continue;
        }
        let Some(cells) = provenance::cells(trimmed) else {
            problems.push(format!("{at}: a row of the table does not end with `|`"));
            continue;
        };
        let [id, name, revision, kind, named_in, baseline, rows, crates] = cells.as_slice() else {
            problems.push(format!(
                "{at}: a row is {} cells, and the list's rows are {}",
                cells.len(),
                COLUMNS.len()
            ));
            continue;
        };
        specs.push(Spec {
            id: id.clone(),
            name: name.clone(),
            revision: revision.clone(),
            kind: kind.clone(),
            named_in: named_in.clone(),
            baseline: entries(baseline),
            selectors: entries(rows),
            crates: entries(crates),
        });
    }
    if tables != 1 {
        problems.push(format!(
            "{FILE} holds {tables} tables headed `{header}`, and it holds one"
        ));
    }
    if problems.is_empty() {
        Ok(specs)
    } else {
        Err(problems)
    }
}

/// The matrix rows a selector names: one row by its ID, or every row whose ID starts with the
/// prefix before a final `*`.
fn selected<'a>(selector: &str, rows: &'a [Row]) -> Vec<&'a Row> {
    match selector.strip_suffix('*') {
        Some(prefix) => rows.iter().filter(|r| r.id.starts_with(prefix)).collect(),
        None => rows.iter().filter(|r| r.id == selector).collect(),
    }
}

/// The matrix rows that rest on `spec`, each once, in the matrix's order.
pub(crate) fn rows_of<'a>(spec: &Spec, rows: &'a [Row]) -> Vec<&'a Row> {
    let wanted: BTreeSet<&str> = spec
        .selectors
        .iter()
        .flat_map(|s| selected(s, rows))
        .map(|r| r.id.as_str())
        .collect();
    rows.iter()
        .filter(|r| wanted.contains(r.id.as_str()))
        .collect()
}

/// Whether `id` is `SP-` and two or more digits.
fn is_id(id: &str) -> bool {
    id.strip_prefix("SP-")
        .is_some_and(|n| n.len() >= 2 && n.bytes().all(|b| b.is_ascii_digit()))
}

/// What is wrong with the list, against the matrix's rows and the crates the provenance table
/// names.
pub(crate) fn check(specs: &[Spec], rows: &[Row], crates: &BTreeSet<String>) -> Vec<String> {
    let mut problems = Vec::new();
    let mut ids = BTreeSet::new();
    let mut named_baseline = BTreeSet::new();
    let baseline_rows: BTreeSet<&str> = rows
        .iter()
        .filter(|r| r.requirement().starts_with(BASELINE))
        .map(|r| r.id.as_str())
        .collect();
    for spec in specs {
        let at = format!("{FILE}: {}", spec.id);
        if !is_id(&spec.id) {
            problems.push(format!("{at}: an ID is `SP-` and two or more digits"));
        }
        if !ids.insert(spec.id.as_str()) {
            problems.push(format!("{at}: the ID is used twice"));
        }
        if spec.name.is_empty() || spec.revision.is_empty() || spec.named_in.is_empty() {
            problems.push(format!(
                "{at}: the specification, its revision and where it is named are never empty"
            ));
        }
        if !KINDS.contains(&spec.kind.as_str()) {
            problems.push(format!(
                "{at}: the kind `{}` is none of {}",
                spec.kind,
                KINDS.join(", ")
            ));
        }
        for id in &spec.baseline {
            if baseline_rows.contains(id.as_str()) {
                named_baseline.insert(id.as_str());
            } else {
                problems.push(format!(
                    "{at}: {id} is not one of §17's baseline statements (`{BASELINE} …`)"
                ));
            }
        }
        for selector in &spec.selectors {
            if selected(selector, rows).is_empty() {
                problems.push(format!("{at}: `{selector}` names no row of the matrix"));
            }
        }
        for name in &spec.crates {
            if !crates.contains(name) {
                problems.push(format!(
                    "{at}: the crate `{name}` has no row in {LEDGER}'s table"
                ));
            }
        }
    }
    for id in baseline_rows.difference(&named_baseline) {
        problems.push(format!(
            "{FILE}: §17's baseline statement {id} is named by no specification"
        ));
    }
    problems
}

/// The crates the provenance table has a row for: the name in its first cell, `` `name`
/// (`path`) ``.
pub(crate) fn crates_of(ledger: &str) -> BTreeSet<String> {
    ledger
        .lines()
        .filter_map(provenance::cells)
        .filter_map(|cells| {
            let first = cells.first()?;
            let (name, rest) = first.strip_prefix('`')?.split_once('`')?;
            rest.trim().starts_with("(`").then(|| name.to_string())
        })
        .collect()
}

/// The list as the checkout holds it, checked against `rows` and the provenance table.
pub(crate) fn read(rows: &[Row]) -> Result<Vec<Spec>> {
    let root = repo_root();
    let text = fs::read_to_string(root.join(FILE)).map_err(|e| format!("{FILE}: {e}"))?;
    let ledger = fs::read_to_string(root.join(LEDGER)).map_err(|e| format!("{LEDGER}: {e}"))?;
    let specs = parse(&text).map_err(|p| p.join("\n  "))?;
    let problems = check(&specs, rows, &crates_of(&ledger));
    if problems.is_empty() {
        Ok(specs)
    } else {
        Err(format!(
            "the list of specifications is not true to the tree:\n  {}",
            problems.join("\n  ")
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::requirements;

    const HEADER: &str = "| ID | Requirement | Kind | Scope | Owner | State | Implementation | Acceptance | Evidence | Env | Review | Limitations | Decided by |\n|---|---|---|---|---|---|---|---|---|---|---|---|---|\n";

    /// A matrix of three rows: one baseline statement of §17 and two rows of §33.
    fn rows() -> Vec<Row> {
        let text = format!(
            "{HEADER}| S17-07 | Initial reference baseline: NVMe Base 2.4. | rule | eng/host | 27 | not-started | - | - | - | - | none | - | - |\n| S33-01 | Implement: Controller discovery. | build | both/hw | 57 | not-started | - | - | - | - | none | - | - |\n| S33-02 | Implement: Initialization. | build | both/hw | 57 | not-started | - | - | - | - | none | - | - |\n"
        );
        let mut problems = Vec::new();
        let rows = requirements::matrix_rows(
            &[("directive-04-research-and-governance.md".into(), text)],
            &mut problems,
        );
        assert!(problems.is_empty(), "{problems:?}");
        rows
    }

    fn list(rows: &str) -> String {
        format!(
            "# Specifications\n\n| ID | Specification | Revision | Kind | Named in | Baseline | Rows | Crates |\n|---|---|---|---|---|---|---|---|\n{rows}"
        )
    }

    fn crates() -> BTreeSet<String> {
        ["framekernel".to_string()].into()
    }

    const NVME: &str = "| SP-01 | NVM Express Base | 2.4 | standard | CLAUDE.md §17 | S17-07 | S33-* | framekernel |\n";

    #[test]
    fn a_good_list_reads_and_its_prefix_selects_its_rows() {
        let specs = parse(&list(NVME)).expect("a good list");
        let rows = rows();
        assert!(check(&specs, &rows, &crates()).is_empty());
        let ids: Vec<&str> = rows_of(&specs[0], &rows)
            .iter()
            .map(|r| r.id.as_str())
            .collect();
        assert_eq!(ids, ["S33-01", "S33-02"]);
    }

    #[test]
    fn a_row_named_twice_is_counted_once() {
        let specs = parse(&list(
            "| SP-01 | NVM Express Base | 2.4 | standard | CLAUDE.md §17 | S17-07 | S33-01, S33-*, S33-01 | - |\n",
        ))
        .expect("a good list");
        assert_eq!(rows_of(&specs[0], &rows()).len(), 2);
    }

    #[test]
    fn a_baseline_statement_no_specification_names_is_refused() {
        let specs = parse(&list(
            "| SP-01 | NVM Express Base | 2.4 | standard | CLAUDE.md §17 | - | S33-* | - |\n",
        ))
        .expect("a good list");
        let problems = check(&specs, &rows(), &crates());
        assert!(
            problems
                .iter()
                .any(|p| p.contains("S17-07 is named by no specification")),
            "{problems:?}"
        );
    }

    #[test]
    fn a_baseline_cell_naming_another_row_is_refused() {
        let specs = parse(&list(
            "| SP-01 | NVM Express Base | 2.4 | standard | CLAUDE.md §17 | S17-07, S33-01 | S33-* | - |\n",
        ))
        .expect("a good list");
        let problems = check(&specs, &rows(), &crates());
        assert!(
            problems
                .iter()
                .any(|p| p.contains("S33-01 is not one of §17's baseline statements")),
            "{problems:?}"
        );
    }

    #[test]
    fn a_row_or_prefix_that_names_nothing_is_refused() {
        for selector in ["S33-99", "S34-*"] {
            let specs = parse(&list(&format!(
                "| SP-01 | NVM Express Base | 2.4 | standard | CLAUDE.md §17 | S17-07 | {selector} | - |\n"
            )))
            .expect("a good list");
            let problems = check(&specs, &rows(), &crates());
            assert!(
                problems
                    .iter()
                    .any(|p| p.contains(&format!("`{selector}` names no row"))),
                "{selector}: {problems:?}"
            );
        }
    }

    #[test]
    fn an_unknown_crate_a_bad_kind_and_a_repeated_id_are_refused() {
        let specs = parse(&list(&format!(
            "{NVME}| SP-01 | Something | 1 | rumour | here | - | - | not-a-crate |\n"
        )))
        .expect("a good list");
        let problems = check(&specs, &rows(), &crates());
        for wanted in [
            "is used twice",
            "the kind `rumour`",
            "the crate `not-a-crate`",
        ] {
            assert!(
                problems.iter().any(|p| p.contains(wanted)),
                "{wanted}: {problems:?}"
            );
        }
    }

    #[test]
    fn a_bad_id_is_refused() {
        let specs = parse(&list(
            "| SP-1 | NVM Express Base | 2.4 | standard | CLAUDE.md §17 | S17-07 | S33-* | - |\n",
        ))
        .expect("a good list");
        assert!(
            check(&specs, &rows(), &crates())
                .iter()
                .any(|p| p.contains("an ID is `SP-`"))
        );
    }

    #[test]
    fn a_short_row_and_a_missing_table_are_refused() {
        let problems = parse(&list("| SP-01 | NVM Express Base | 2.4 |\n")).expect_err("short");
        assert!(
            problems.iter().any(|p| p.contains("is 3 cells")),
            "{problems:?}"
        );
        let problems = parse("# Specifications\n\nNo table.\n").expect_err("no table");
        assert!(
            problems.iter().any(|p| p.contains("holds 0 tables")),
            "{problems:?}"
        );
    }

    #[test]
    fn the_provenance_tables_first_cells_name_the_crates() {
        let ledger = "| Module name | Authoring |\n|---|---|\n| `framekernel` (`framekernel`) | Claude |\n| `uefi_smoke.rs` | Claude |\n| `origin-aml` (`firmware/acpi/aml`) | Claude |\n";
        let found = crates_of(ledger);
        assert_eq!(
            found,
            ["framekernel".to_string(), "origin-aml".to_string()].into()
        );
    }

    #[test]
    fn the_trees_list_is_true_to_the_tree() {
        let rows = crate::roadmap::matrix().expect("the matrix");
        read(&rows).expect("the tree's list of specifications");
    }
}
