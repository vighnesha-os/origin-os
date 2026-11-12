//! `cargo xtask check`'s gate on the provenance table (CLAUDE.md §6; TASKS item 25; decision
//! item 122).
//!
//! §6 asks for the same fields of every original module and says to "keep these fields in a
//! table with one row per crate, which a gate in `cargo xtask check` reads. The narrative entries
//! are its evidence." The table is a section of `docs/engineering/SOURCE_PROVENANCE.md` of its
//! own ([`SECTION`]), with a column for each of §6's eleven fields ([`FIELDS`]). The narrative
//! entries around it are not read here.
//!
//! **What it refuses:**
//!
//! - a crate of the tree with no row: every `Cargo.toml` that has a `[package]` table is one,
//!   whether or not the workspace lists it;
//! - a row with an empty field: a cell with nothing in it, or only a placeholder such as `-` or
//!   `TBD` ([`PLACEHOLDERS`]). A field with nothing to record says so in words: "none", "not
//!   recorded";
//! - a row that names no crate: its first cell does not hold a package name and its directory, in
//!   backticks, that a manifest of the tree has; and a crate with two rows;
//! - a row whose "Source dependencies" cell does not name, in backticks, each crate its manifest
//!   depends on, in any dependency table; and a row whose "Licensing status" cell does not hold
//!   the licence expression its manifest declares (§6: "automated provenance and license checks
//!   where practical");
//! - and, failing closed: no table, or a second one, in the section; a section missing or given
//!   twice; a header that is not [`FIELDS`]; a row with another number of cells; a manifest that
//!   cannot be read, or whose package has no name or no licence the gate can find; a run that
//!   found no crate or read no row.
//!
//! **What it does not see:** whether a cell is true. A review, a specification or a test named
//! in a row is the narrative's claim, and the narrative is the evidence; the gate checks only
//! that every crate has every field filled, and that two of them agree with the manifest.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;

use crate::{Result, collect_files, repo_root};

/// The file that holds the table, beside the narrative entries that are its evidence.
const LEDGER: &str = "docs/engineering/SOURCE_PROVENANCE.md";

/// The heading of the table's section. The table is the one table between it and the next
/// heading of the same level.
const SECTION: &str = "## The provenance table (CLAUDE.md §6; decision item 122)";

/// The table's header: CLAUDE.md 7.6 §6's fields, in its order. The eleventh came with revision
/// 7.3 (decision items 93, 96, 97).
const FIELDS: [&str; 11] = [
    "Module name",
    "Authoring agent or contributor",
    "Applicable specifications",
    "Architecture decision",
    "Source dependencies",
    "Toolchain requirements",
    "Code review",
    "Licensing status",
    "Test evidence",
    "Known external firmware/data dependencies",
    "Open-source references consulted (§3)",
];

/// The column whose cell must name each dependency in the crate's manifest.
const DEPENDENCIES: usize = 4;
/// The column whose cell must hold the licence expression of the crate's manifest.
const LICENCE: usize = 7;

/// Cell values that stand for nothing. Matched after trimming, ignoring case.
const PLACEHOLDERS: &[&str] = &[
    "", "-", "–", "—", "?", "...", "…", "tbd", "todo", "n/a", "tba", "fixme", "xxx",
];

/// A crate of the tree, as its manifest declares it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Crate {
    /// `[package] name`.
    name: String,
    /// Its directory, from the repository's root, with `/` between components.
    directory: String,
    /// Every crate named in a dependency table of its manifest, of any kind.
    dependencies: BTreeSet<String>,
    /// The licence expression, from `license` or the workspace's `license`.
    licence: String,
}

/// The text inside double quotes in `value`, if it is one quoted string with no escape.
fn quoted(value: &str) -> Option<&str> {
    let inner = value.trim().strip_prefix('"')?.strip_suffix('"')?;
    (!inner.contains(['"', '\\'])).then_some(inner)
}

/// The key of a `key = value` or `key.field = value` line, and its value.
fn key_value(line: &str) -> Option<(&str, &str)> {
    let (key, value) = line.split_once('=')?;
    let key = key.trim();
    let key = key
        .split('.')
        .next()
        .unwrap_or(key)
        .trim()
        .trim_matches('"');
    (!key.is_empty()).then_some((key, value.trim()))
}

/// Whether a table name is a dependency table: `dependencies`, `dev-dependencies`,
/// `build-dependencies`, alone or under `target.<cfg>.`. Returns the dependency it names, if the
/// table is one dependency's (`[dependencies.foo]`), and `Some(None)` for a whole table.
fn dependency_table(table: &str) -> Option<Option<&str>> {
    let table = match table.strip_prefix("target.") {
        // `target.'cfg(…)'.dependencies`: the kind is after the last quote. A bare triple, as in
        // `target.x86_64-unknown-none.dependencies`, has no dot of its own.
        Some(rest) => match rest.rsplit_once(['\'', '"']) {
            Some((_, kind)) => kind,
            None => rest.split_once('.').map_or(rest, |(_, kind)| kind),
        },
        None => table,
    };
    let table = table.trim_start_matches('.');
    for kind in ["dependencies", "dev-dependencies", "build-dependencies"] {
        if table == kind {
            return Some(None);
        }
        if let Some(name) = table.strip_prefix(kind).and_then(|r| r.strip_prefix('.')) {
            return Some(Some(name.trim_matches('"')));
        }
    }
    None
}

/// What a manifest declares, or why it cannot be read. `Ok(None)` for a manifest with no
/// `[package]` table, such as the workspace's root. `workspace_licence` is the root's
/// `[workspace.package] license`, which `license.workspace = true` takes.
fn manifest(
    directory: &str,
    text: &str,
    workspace_licence: Option<&str>,
) -> std::result::Result<Option<Crate>, String> {
    let mut table = String::new();
    let mut package = false;
    let mut name = None;
    let mut licence = None;
    let mut dependencies = BTreeSet::new();
    for line in text.lines() {
        let line = line.split_once(" #").map_or(line, |(code, _)| code).trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') {
            table = line.trim_matches(['[', ']']).trim().to_string();
            package |= table == "package";
            if let Some(Some(dependency)) = dependency_table(&table) {
                dependencies.insert(dependency.to_string());
            }
            continue;
        }
        let Some((key, value)) = key_value(line) else {
            continue;
        };
        match (table.as_str(), key) {
            ("package", "name") => {
                name = Some(quoted(value).ok_or_else(|| {
                    format!("{directory}/Cargo.toml: a package name the gate cannot read: {line}")
                })?);
            }
            ("package", "license") => {
                licence = if line.contains("workspace") {
                    workspace_licence
                } else {
                    quoted(value)
                };
            }
            (table, dependency) if dependency_table(table) == Some(None) => {
                dependencies.insert(dependency.to_string());
            }
            _ => {}
        }
    }
    if !package {
        return Ok(None);
    }
    let name = name.ok_or_else(|| format!("{directory}/Cargo.toml: a package with no name"))?;
    let licence = licence.ok_or_else(|| {
        format!("{directory}/Cargo.toml: package `{name}` declares no licence the gate can read")
    })?;
    Ok(Some(Crate {
        name: name.to_string(),
        directory: directory.to_string(),
        dependencies,
        licence: licence.to_string(),
    }))
}

/// The root's `[workspace.package] license`, if it has one.
fn workspace_licence(root_manifest: &str) -> Option<&str> {
    let mut table = "";
    for line in root_manifest.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            table = line.trim_matches(['[', ']']).trim();
        } else if table == "workspace.package"
            && let Some(("license", value)) = key_value(line)
        {
            return quoted(value);
        }
    }
    None
}

/// The cells of one table line, trimmed, with `\|` read as `|`. `None` if the line does not
/// start and end with a `|`.
pub(crate) fn cells(line: &str) -> Option<Vec<String>> {
    let rest = line.trim().strip_prefix('|')?;
    let mut cells = Vec::new();
    let mut cell = String::new();
    let mut chars = rest.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' if chars.peek() == Some(&'|') => {
                chars.next();
                cell.push('|');
            }
            '|' => cells.push(std::mem::take(&mut cell).trim().to_string()),
            c => cell.push(c),
        }
    }
    (cell.trim().is_empty() && !cells.is_empty()).then_some(cells)
}

/// The spans of `text` between backticks, in order.
fn code_spans(text: &str) -> Vec<&str> {
    text.split('`').skip(1).step_by(2).collect()
}

/// A body row of the table: its line number, from 1, and its cells.
type Row = (usize, Vec<String>);

/// The table of the section, or why there is none to read.
fn table(ledger: &str) -> std::result::Result<Vec<Row>, String> {
    let level = SECTION.split(' ').next().unwrap_or("##");
    let mut sections = 0;
    let mut inside = false;
    let mut fenced = false;
    let mut tables: Vec<Vec<Row>> = Vec::new();
    let mut open = false;
    for (index, line) in ledger.lines().enumerate() {
        if line.trim_start().starts_with("```") {
            fenced = !fenced;
            open = false;
            continue;
        }
        if fenced {
            continue;
        }
        if line.trim_end() == SECTION {
            sections += 1;
            inside = true;
            open = false;
            continue;
        }
        let heading = line.split(' ').next().unwrap_or("");
        if inside && !heading.is_empty() && heading.chars().all(|c| c == '#') {
            // A heading at the section's level or above ends it; a deeper one is inside it.
            if heading.len() <= level.len() {
                inside = false;
            }
            open = false;
            continue;
        }
        if !inside || !line.trim_start().starts_with('|') {
            open = false;
            continue;
        }
        let row = cells(line).unwrap_or_else(|| vec![line.to_string()]);
        match tables.last_mut() {
            Some(table) if open => {
                let rule = row
                    .iter()
                    .all(|c| !c.is_empty() && c.chars().all(|ch| matches!(ch, '-' | ':')));
                if !(rule && table.len() == 1) {
                    table.push((index + 1, row));
                }
            }
            _ => {
                tables.push(vec![(index + 1, row)]);
                open = true;
            }
        }
    }
    match (sections, tables.len()) {
        (0, _) => return Err(format!("{LEDGER} has no section `{SECTION}`")),
        (1, 1) => {}
        (1, 0) => {
            return Err(format!(
                "the section `{SECTION}` of {LEDGER} holds no table"
            ));
        }
        (1, n) => {
            return Err(format!(
                "the section `{SECTION}` of {LEDGER} holds {n} tables; the provenance table must be the only one"
            ));
        }
        (n, _) => return Err(format!("{LEDGER} has the section `{SECTION}` {n} times")),
    }
    let mut table = tables.pop().unwrap_or_default();
    let (line, header) = table.remove(0);
    if !(header.len() == FIELDS.len() && header.iter().zip(FIELDS).all(|(h, f)| h == f)) {
        return Err(format!(
            "{LEDGER}:{line}: the provenance table's header must be §6's fields, `| {} |`",
            FIELDS.join(" | ")
        ));
    }
    Ok(table)
}

/// Whether a cell stands for nothing.
fn is_empty(cell: &str) -> bool {
    let cell = cell.trim().trim_matches('*').trim();
    PLACEHOLDERS.iter().any(|p| cell.eq_ignore_ascii_case(p))
}

/// What is wrong with the table, given the crates of the tree.
fn problems(rows: &[Row], crates: &[Crate]) -> Vec<String> {
    let mut problems = Vec::new();
    if crates.is_empty() {
        problems.push("no crate was found in the tree: nothing was checked".to_string());
    }
    if rows.is_empty() {
        problems.push(format!("the provenance table in {LEDGER} has no row"));
    }
    let by_name: BTreeMap<&str, &Crate> = crates.iter().map(|c| (c.name.as_str(), c)).collect();
    let mut rows_of: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for (line, cells) in rows {
        let at = format!("{LEDGER}:{line}");
        if cells.len() != FIELDS.len() {
            problems.push(format!(
                "{at}: a row of {} cells; the table has {}",
                cells.len(),
                FIELDS.len()
            ));
            continue;
        }
        for (field, cell) in FIELDS.iter().zip(cells) {
            if is_empty(cell) {
                problems.push(format!(
                    "{at}: the field \"{field}\" is empty; a field with nothing to record says so in words"
                ));
            }
        }
        let spans = code_spans(&cells[0]);
        let named = match spans.as_slice() {
            [name, directory, ..] => by_name
                .get(name)
                .filter(|c| c.directory == *directory)
                .copied(),
            _ => None,
        };
        let Some(krate) = named else {
            problems.push(format!(
                "{at}: the row names no crate of the tree: its first cell must be `` `<package name>` (`<directory>`) `` as a Cargo.toml declares them, not {:?}",
                cells[0]
            ));
            continue;
        };
        rows_of.entry(krate.name.as_str()).or_default().push(*line);
        let listed: BTreeSet<&str> = code_spans(&cells[DEPENDENCIES]).into_iter().collect();
        for dependency in &krate.dependencies {
            if !listed.contains(dependency.as_str()) {
                problems.push(format!(
                    "{at}: `{}` depends on `{dependency}` (its Cargo.toml), and the field \"{}\" does not name it in backticks",
                    krate.name, FIELDS[DEPENDENCIES]
                ));
            }
        }
        if !cells[LICENCE].contains(&krate.licence) {
            problems.push(format!(
                "{at}: `{}` is licensed `{}` by its Cargo.toml, and the field \"{}\" does not say so",
                krate.name, krate.licence, FIELDS[LICENCE]
            ));
        }
    }
    for krate in crates {
        match rows_of.get(krate.name.as_str()).map(Vec::as_slice) {
            None | Some([]) => problems.push(format!(
                "the crate `{}` (`{}`) has no row in the provenance table of {LEDGER}",
                krate.name, krate.directory
            )),
            Some([_]) => {}
            Some(lines) => problems.push(format!(
                "the crate `{}` has {} rows, at lines {lines:?} of {LEDGER}",
                krate.name,
                lines.len()
            )),
        }
    }
    problems
}

/// The crates of the tree: every `Cargo.toml` outside `target/` and hidden directories that has
/// a `[package]` table.
fn crates() -> Result<Vec<Crate>> {
    let root = repo_root();
    let read = |path: &std::path::Path| {
        fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))
    };
    let root_text = read(&root.join("Cargo.toml"))?;
    let licence = workspace_licence(&root_text);
    let mut manifests = Vec::new();
    collect_files(
        &root,
        &|_| false,
        &|name| name == "Cargo.toml",
        &mut manifests,
    )
    .map_err(|e| format!("provenance gate: {e}"))?;
    let mut crates = Vec::new();
    let mut problems = Vec::new();
    for path in &manifests {
        let directory = path
            .parent()
            .and_then(|p| p.strip_prefix(&root).ok())
            .map(|p| {
                p.components()
                    .map(|c| c.as_os_str().to_string_lossy())
                    .collect::<Vec<_>>()
                    .join("/")
            })
            .ok_or_else(|| format!("{}: outside the repository", path.display()))?;
        match manifest(&directory, &read(path)?, licence) {
            Ok(Some(krate)) => crates.push(krate),
            Ok(None) => {}
            Err(problem) => problems.push(problem),
        }
    }
    if problems.is_empty() {
        Ok(crates)
    } else {
        Err(format!(
            "the provenance gate cannot read every manifest (CLAUDE.md §6):\n  {}",
            problems.join("\n  ")
        ))
    }
}

/// The gate: every crate of the tree has one row of the provenance table, with every one of
/// §6's fields filled, its dependencies named and its licence stated.
pub(crate) fn gate() -> Result {
    let crates = crates()?;
    let path = repo_root().join(LEDGER);
    let ledger = fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let rows = table(&ledger)
        .map_err(|e| format!("the provenance table is not readable (CLAUDE.md §6): {e}"))?;
    let problems = problems(&rows, &crates);
    if !problems.is_empty() {
        return Err(format!(
            "the provenance table does not cover every crate with every field (CLAUDE.md §6, decision item 122; its rules are in developer-tools/xtask/src/provenance.rs):\n  {}",
            problems.join("\n  ")
        ));
    }
    println!(
        "xtask: provenance gate: {} crates, each with one row and §6's {} fields",
        crates.len(),
        FIELDS.len()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A manifest like the workspace's members.
    const MEMBER: &str = r#"[package]
name = "origin-thing"
description = "a thing" # with a comment
edition.workspace = true
license.workspace = true

[dependencies]
origin-version.workspace = true
origin-paging = { path = "../paging" }

[dev-dependencies]
origin-abi = { path = "../../runtime/abi" }

[target.'cfg(loom)'.dependencies]
loom = "0.7"

[build-dependencies.origin-build]
path = "../build"

[lints]
workspace = true
"#;

    const ROOT: &str =
        "[workspace]\nmembers = [\"a\"]\n\n[workspace.package]\nlicense = \"MIT OR Apache-2.0\"\n";

    fn thing() -> Crate {
        manifest("framekernel/thing", MEMBER, workspace_licence(ROOT))
            .expect("readable")
            .expect("a package")
    }

    /// A row for `thing()` with every field filled.
    fn good_row() -> Vec<String> {
        let mut row: Vec<String> = FIELDS.iter().map(|f| format!("about {f}")).collect();
        row[0] = "`origin-thing` (`framekernel/thing`)".into();
        row[DEPENDENCIES] =
            "`origin-version`, `origin-paging`; dev: `origin-abi`; `loom`; build: `origin-build`"
                .into();
        row[LICENCE] = "MIT OR Apache-2.0".into();
        row
    }

    fn ledger(rows: &[Vec<String>]) -> String {
        let mut text = format!(
            "# Source provenance\n\n{SECTION}\n\nThe table.\n\n| {} |\n|{}\n",
            FIELDS.join(" | "),
            "---|".repeat(FIELDS.len())
        );
        for row in rows {
            text.push_str(&format!("| {} |\n", row.join(" | ")));
        }
        text.push_str(
            "\n## `boot/uefi/` (`origin-boot-uefi`)\n\n| Not | The table |\n|---|---|\n| a | b |\n",
        );
        text
    }

    fn found(rows: &[Vec<String>], crates: &[Crate]) -> Vec<String> {
        problems(&table(&ledger(rows)).expect("a table"), crates)
    }

    #[test]
    fn a_manifest_is_read() {
        let krate = thing();
        assert_eq!(krate.name, "origin-thing");
        assert_eq!(krate.directory, "framekernel/thing");
        assert_eq!(krate.licence, "MIT OR Apache-2.0");
        let want: BTreeSet<String> = [
            "loom",
            "origin-abi",
            "origin-build",
            "origin-paging",
            "origin-version",
        ]
        .into_iter()
        .map(String::from)
        .collect();
        assert_eq!(krate.dependencies, want);
        assert_eq!(manifest(".", ROOT, None), Ok(None), "the root is no crate");
    }

    #[test]
    fn a_manifest_it_cannot_read_fails() {
        let no_name = "[package]\nedition = \"2024\"\nlicense = \"MIT\"\n";
        assert!(manifest("x", no_name, None).is_err());
        let escaped = "[package]\nname = \"a\\\"b\"\nlicense = \"MIT\"\n";
        assert!(manifest("x", escaped, None).is_err());
        let no_licence = "[package]\nname = \"a\"\n";
        assert!(manifest("x", no_licence, None).is_err());
        let workspace_but_none = "[package]\nname = \"a\"\nlicense.workspace = true\n";
        assert!(manifest("x", workspace_but_none, None).is_err());
        let own = "[package]\nname = \"a\"\nlicense = \"MIT\"\n";
        assert_eq!(
            manifest("x", own, None).expect("ok").expect("a").licence,
            "MIT"
        );
    }

    #[test]
    fn a_complete_table_passes() {
        assert_eq!(found(&[good_row()], &[thing()]), Vec::<String>::new());
    }

    #[test]
    fn a_crate_with_no_row_is_refused() {
        let mut other = thing();
        other.name = "origin-other".into();
        other.directory = "other".into();
        let problems = found(&[good_row()], &[thing(), other]);
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(
            problems[0].contains("`origin-other` (`other`) has no row"),
            "{problems:?}"
        );
    }

    #[test]
    fn an_empty_field_is_refused_in_every_column() {
        for column in 0..FIELDS.len() {
            for empty in ["", "-", "—", "TBD", "todo", "**?**"] {
                let mut row = good_row();
                row[column] = empty.into();
                let problems = found(&[row], &[thing()]);
                assert!(
                    problems
                        .iter()
                        .any(|p| p.contains(&format!("\"{}\" is empty", FIELDS[column]))),
                    "column {column}, {empty:?}: {problems:?}"
                );
            }
        }
        let mut row = good_row();
        row[10] = "none".into();
        assert_eq!(
            found(&[row], &[thing()]),
            Vec::<String>::new(),
            "\"none\" is a value"
        );
    }

    #[test]
    fn a_row_that_names_no_crate_is_refused() {
        for name in [
            "`origin-nothing` (`framekernel/thing`)",
            "`origin-thing` (`framekernel/elsewhere`)",
            "`origin-thing`",
            "origin-thing (framekernel/thing)",
        ] {
            let mut stray = good_row();
            stray[0] = name.into();
            let problems = found(&[good_row(), stray], &[thing()]);
            assert_eq!(problems.len(), 1, "{name}: {problems:?}");
            assert!(
                problems[0].contains("names no crate"),
                "{name}: {problems:?}"
            );
        }
    }

    #[test]
    fn a_crate_with_two_rows_is_refused() {
        let problems = found(&[good_row(), good_row()], &[thing()]);
        assert!(
            problems.iter().any(|p| p.contains("has 2 rows")),
            "{problems:?}"
        );
    }

    #[test]
    fn a_dependency_not_named_is_refused() {
        let mut row = good_row();
        row[DEPENDENCIES] = "`origin-version`, `origin-paging`; dev: origin-abi".into();
        let problems = found(&[row], &[thing()]);
        assert!(
            problems.iter().any(|p| p.contains("`origin-abi`")),
            "{problems:?}"
        );
        assert!(
            problems.iter().any(|p| p.contains("`loom`")),
            "{problems:?}"
        );
        assert!(
            problems.iter().any(|p| p.contains("`origin-build`")),
            "{problems:?}"
        );
        assert_eq!(problems.len(), 3, "{problems:?}");
    }

    #[test]
    fn a_licence_not_stated_is_refused() {
        let mut row = good_row();
        row[LICENCE] = "MIT".into();
        let problems = found(&[row], &[thing()]);
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].contains("MIT OR Apache-2.0"), "{problems:?}");
    }

    #[test]
    fn a_row_of_the_wrong_width_is_refused() {
        let mut row = good_row();
        row.pop();
        let problems = found(&[row], &[thing()]);
        assert!(
            problems.iter().any(|p| p.contains("a row of 10 cells")),
            "{problems:?}"
        );
    }

    #[test]
    fn the_table_must_be_there_once_with_its_header() {
        assert!(
            table("# Source provenance\n\nNo table.\n").is_err(),
            "no section"
        );
        let empty = format!("{SECTION}\n\nNothing here.\n\n## Next\n\n| a |\n|---|\n| b |\n");
        assert!(table(&empty).is_err(), "no table in the section");
        let twice = format!("{}\n{}", ledger(&[good_row()]), ledger(&[good_row()]));
        assert!(table(&twice).is_err(), "the section twice");
        let two = ledger(&[good_row()]).replacen(
            "\n## `boot",
            "\nBetween.\n\n| x |\n|---|\n| y |\n\n## `boot",
            1,
        );
        assert!(table(&two).is_err(), "two tables in the section");
        let header = ledger(&[good_row()]).replacen("| Code review |", "| Review |", 1);
        assert!(table(&header).is_err(), "a header that is not §6's fields");
        let fenced = ledger(&[good_row()]).replacen(SECTION, &format!("```\n{SECTION}\n```"), 1);
        assert!(
            table(&fenced).is_err(),
            "a heading inside a fence is no section"
        );
    }

    #[test]
    fn nothing_read_is_a_failure() {
        assert!(!found(&[], &[thing()]).is_empty(), "no row");
        assert!(!found(&[good_row()], &[]).is_empty(), "no crate");
    }

    #[test]
    fn the_tree_passes() {
        gate().expect("the provenance table of this tree");
    }
}
