//! `cargo xtask check`'s gate on the cost ledger (ADR-0018 V5 and V8; decision items 242 and
//! 245; TASKS item 54).
//!
//! ADR-0018 V5 keeps one cost ledger, in `docs/engineering/PERFORMANCE_BUDGETS.md`: a row for
//! each place the design pays on each operation, or pays a price nobody has measured. V3 asks
//! every design decision accepted after ADR-0018 to state its cost in one of three forms, "None,
//! by construction", "Measured" or "Unmeasured, owed", and V8 asks for this gate.
//!
//! **What it reads.** The ledger section of the budgets file ([`SECTION`]) holds two tables: the
//! ledger itself, under [`LEDGER_COLUMNS`], and the list of ADRs, under [`LIST_COLUMNS`]. Each
//! ledger row has an ID, `CL` and a number. A "Performance cost" statement is a table row whose
//! first cell is "Performance cost", or a paragraph that starts `**Performance cost`. An ADR or
//! an amendment is held to state its cost if it was accepted after ADR-0018: an ADR numbered
//! from 18 whose status is Accepted, or an amendment past the ones its ADR held when ADR-0018 was
//! accepted ([`BEFORE`]) whose heading does not say "Proposed".
//!
//! **What it refuses** (V8's three faults, and what keeps them checkable):
//!
//! - an ADR or an amendment held to state its cost that has no "Performance cost" statement, or
//!   whose statement is in none of V3's forms ([`forms`]);
//! - a statement in the "Measured" or "Unmeasured, owed" form that names no ledger row;
//! - a ledger ID named anywhere in an ADR's text, held or not, that is not a row of the ledger,
//!   so that a reference from an older ADR's evidence is checked too;
//! - a ledger row whose "What the cost buys" names an ADR decision that does not exist, a section
//!   of the directive that does not exist or a threat model that is not in the tree, or names no
//!   ADR decision and no section at all;
//! - a ledger row whose figure names no environment: a "Measured, and where" cell that does not
//!   start "Unmeasured" and names none of `emu`, `virt` and `hw`, the requirements matrix's words;
//! - a list that leaves out one of ADR-0001 to ADR-0017, names one twice, says "none" with no
//!   reason, or names a row that the ledger does not hold or whose "What the cost buys" does not
//!   name that ADR;
//! - a ledger row that the list does not name, or does not name under every ADR from 1 to 17 its
//!   "What the cost buys" names;
//! - a ledger row with an empty cell or a malformed or repeated ID;
//! - and, failing closed: a budgets file with no ledger section, a section with no ledger or no
//!   list, or a ledger with no row.
//!
//! **What it does not see:** whether the ledger is complete, which only a reading of the ADRs can
//! say; whether an estimate or a figure is right; whether a cheaper design was missed; an ADR's
//! text corrected in place after ADR-0018 without a new amendment, as ADR-0009's launch dates
//! were (items 257 and 258), which changes facts and not a design; and ADR-0001 to ADR-0017's own
//! texts, which V8 leaves unedited.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;

use crate::provenance::cells;
use crate::requirements;
use crate::{Result, repo_root};

/// The file that holds the ledger.
const BUDGETS: &str = "docs/engineering/PERFORMANCE_BUDGETS.md";

/// The section of [`BUDGETS`] that holds the ledger and the list of ADRs.
const SECTION: &str = "## The cost ledger";

/// The ledger's header: an ID, then ADR-0018 V5's six columns.
const LEDGER_COLUMNS: [&str; 7] = [
    "Row",
    "Path",
    "What the cost buys",
    "Estimate, and its model",
    "Measured, and where",
    "Cheaper design considered",
    "What would lift it",
];

/// The list's header: each ADR, its rows or "none", and the reason for none.
const LIST_COLUMNS: [&str; 3] = ["ADR", "Rows", "Why none"];

/// The environments a figure names, as the requirements matrix writes them.
const ENVIRONMENTS: [&str; 3] = ["`emu`", "`virt`", "`hw`"];

/// The ADRs V8 asks the list to name.
const LISTED: std::ops::RangeInclusive<u16> = 1..=17;

/// ADR-0018: from it on, a whole ADR states its cost.
const POLICY: u16 = 18;

/// How many amendments each ADR before [`POLICY`] held when ADR-0018 was accepted, on 2026-11-12
/// (items 238 to 246). An amendment numbered past its ADR's count was accepted after ADR-0018.
/// The first two so accepted are ADR-0005's Amendment 9 and ADR-0011's Amendment 3 (2026-11-13).
const BEFORE: [(u16, u32); 17] = [
    (1, 0),
    (2, 1),
    (3, 0),
    (4, 2),
    (5, 8),
    (6, 0),
    (7, 0),
    (8, 0),
    (9, 1),
    (10, 2),
    (11, 2),
    (12, 0),
    (13, 1),
    (14, 1),
    (15, 0),
    (16, 2),
    (17, 1),
];

/// Which of V3's forms a statement uses. A statement may use several, one for each decision it
/// covers.
#[derive(Debug, Default, PartialEq, Eq)]
struct Forms {
    none: bool,
    owed: bool,
    measured: bool,
}

/// The forms of V3 that `statement` uses. "None, by construction" and "Unmeasured, owed" are
/// found in any case ("Unmeasured and owed" counts as the second); "Measured" only with its
/// capital, so that the "measured" inside "unmeasured" is not taken for it.
fn forms(statement: &str) -> Forms {
    let lower = statement.to_lowercase();
    Forms {
        none: lower.contains("none, by construction"),
        owed: lower.contains("unmeasured, owed") || lower.contains("unmeasured and owed"),
        measured: statement.contains("Measured"),
    }
}

/// The text of a "Performance cost" statement on `line`, if the line holds one: a table row whose
/// first cell is "Performance cost", or a paragraph that starts `**Performance cost`.
fn statement(line: &str) -> Option<String> {
    let line = line.trim();
    if line.starts_with('|') {
        let cells = cells(line)?;
        return (cells.first().map(String::as_str) == Some("Performance cost"))
            .then(|| cells[1..].join(" | "));
    }
    let rest = line.strip_prefix("**Performance cost")?;
    let (_, after) = rest.split_once("**")?;
    Some(after.trim().to_string())
}

/// Whether `bytes[at]` is preceded by a byte that would make a word of it.
fn joined_before(bytes: &[u8], at: usize) -> bool {
    at > 0 && (bytes[at - 1].is_ascii_alphanumeric() || bytes[at - 1] == b'_')
}

/// The ledger IDs `text` names: `CL` and a number, standing alone.
fn ledger_ids(text: &str) -> Vec<String> {
    let bytes = text.as_bytes();
    let mut ids = Vec::new();
    let mut at = 0;
    while let Some(found) = text[at..].find("CL") {
        let start = at + found;
        let digits = bytes[start + 2..]
            .iter()
            .take_while(|b| b.is_ascii_digit())
            .count();
        let end = start + 2 + digits;
        if digits > 0
            && !joined_before(bytes, start)
            && bytes.get(end).is_none_or(|b| !b.is_ascii_alphanumeric())
        {
            ids.push(text[start..end].to_string());
        }
        at = start + 2;
    }
    ids
}

/// Whether `id` is a ledger row's ID: `CL` and a number with no leading zero.
fn is_ledger_id(id: &str) -> bool {
    id.strip_prefix("CL").is_some_and(|n| {
        !n.is_empty() && !n.starts_with('0') && n.bytes().all(|b| b.is_ascii_digit())
    })
}

/// The ADR decisions `text` names, as `ADR-NNNN X1`: each ADR's number and the decision's ID
/// (letters, then digits: `Z3`, `def5`, `Am1`). `ADR-NNNN` followed by anything else names no
/// decision.
fn decision_refs(text: &str) -> Vec<(u16, String)> {
    let mut refs = Vec::new();
    let mut at = 0;
    while let Some(found) = text[at..].find("ADR-") {
        let start = at + found + 4;
        at = start;
        let digits: String = text[start..]
            .chars()
            .take_while(char::is_ascii_digit)
            .collect();
        if digits.len() != 4 {
            continue;
        }
        let Some(rest) = text[start + 4..].strip_prefix(' ') else {
            continue;
        };
        let token: String = rest
            .chars()
            .take_while(char::is_ascii_alphanumeric)
            .collect();
        let letters = token.chars().take_while(char::is_ascii_alphabetic).count();
        let is_id = letters > 0
            && letters < token.len()
            && token[letters..].chars().all(|c| c.is_ascii_digit());
        if is_id && let Ok(number) = digits.parse() {
            refs.push((number, token));
        }
    }
    refs
}

/// The sections of the directive `text` names, as `§N`.
fn section_refs(text: &str) -> Vec<u8> {
    text.split('§')
        .skip(1)
        .filter_map(|rest| {
            let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
            digits.parse().ok()
        })
        .collect()
}

/// The threat models `text` names, as `THREAT_MODEL_NAME.md`: their paths in the tree.
fn threat_model_refs(text: &str) -> Vec<String> {
    let mut refs = Vec::new();
    let mut at = 0;
    while let Some(found) = text[at..].find("THREAT_MODEL_") {
        let start = at + found;
        let name: String = text[start..]
            .chars()
            .take_while(|c| c.is_ascii_uppercase() || *c == '_')
            .collect();
        at = start + name.len();
        if text[at..].starts_with(".md") {
            refs.push(format!("docs/engineering/{name}.md"));
        }
    }
    refs
}

/// A table: its header and its body rows, each with its line number from 1.
struct Table {
    header: Vec<String>,
    rows: Vec<(usize, Vec<String>)>,
}

/// The tables of [`SECTION`] in `budgets`, or `None` if it has no such section.
fn section_tables(budgets: &str) -> Option<Vec<Table>> {
    let mut inside = None;
    let mut fenced = false;
    let mut tables: Vec<Table> = Vec::new();
    let mut in_table = false;
    for (index, line) in budgets.lines().enumerate() {
        let line = line.trim_end();
        if line.starts_with("```") {
            fenced = !fenced;
            in_table = false;
            continue;
        }
        if fenced {
            continue;
        }
        if line.starts_with("## ") {
            if inside == Some(true) {
                break;
            }
            inside = Some(line == SECTION);
            in_table = false;
            continue;
        }
        if inside != Some(true) {
            continue;
        }
        let Some(row) = line.starts_with('|').then(|| cells(line)).flatten() else {
            in_table = false;
            continue;
        };
        if !in_table {
            in_table = true;
            tables.push(Table {
                header: row,
                rows: Vec::new(),
            });
        } else if !row
            .iter()
            .all(|c| !c.is_empty() && c.bytes().all(|b| b == b'-' || b == b':'))
            && let Some(table) = tables.last_mut()
        {
            table.rows.push((index + 1, row));
        }
    }
    (inside == Some(true)).then_some(tables)
}

/// One part of an ADR that V8 holds to state its cost: "the ADR" or "Amendment N", the line it
/// starts on, and the statements in it, each with its line.
struct Held {
    part: String,
    line: usize,
    statements: Vec<(usize, String)>,
}

/// The parts of ADR `number` that V8 holds to state their cost.
fn held_parts(number: u16, accepted: bool, text: &str) -> Vec<Held> {
    struct Amendment {
        number: u32,
        level: usize,
        proposed: bool,
        line: usize,
        statements: Vec<(usize, String)>,
    }
    let before = BEFORE.iter().find(|(n, _)| *n == number).map(|(_, c)| *c);
    let mut base: Vec<(usize, String)> = Vec::new();
    let mut amendments: Vec<Amendment> = Vec::new();
    let mut open: Option<Amendment> = None;
    let mut fenced = false;
    for (index, raw) in text.lines().enumerate() {
        let line = raw.trim_end();
        if line.starts_with("```") {
            fenced = !fenced;
            continue;
        }
        if fenced {
            continue;
        }
        if line.starts_with('#') {
            let level = line.bytes().take_while(|&b| b == b'#').count();
            let heading = line[level..].trim();
            if open.as_ref().is_some_and(|a| level <= a.level) {
                amendments.extend(open.take());
            }
            if (level == 2 || level == 3)
                && let Some(rest) = heading.strip_prefix("Amendment ")
            {
                let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
                if let Ok(amendment) = digits.parse() {
                    amendments.extend(open.take());
                    open = Some(Amendment {
                        number: amendment,
                        level,
                        proposed: heading.contains("Proposed"),
                        line: index + 1,
                        statements: Vec::new(),
                    });
                }
            }
            continue;
        }
        if let Some(found) = statement(line) {
            match open.as_mut() {
                Some(amendment) => amendment.statements.push((index + 1, found)),
                None => base.push((index + 1, found)),
            }
        }
    }
    amendments.extend(open.take());

    let mut held = Vec::new();
    if number >= POLICY && accepted {
        held.push(Held {
            part: "the ADR".to_string(),
            line: 1,
            statements: base,
        });
    }
    for amendment in amendments {
        let later = match before {
            Some(count) => amendment.number > count,
            None => number >= POLICY && accepted,
        };
        if later && !amendment.proposed {
            held.push(Held {
                part: format!("Amendment {}", amendment.number),
                line: amendment.line,
                statements: amendment.statements,
            });
        }
    }
    held
}

/// What the gate counted, for its report.
#[derive(Debug, Default)]
struct Figure {
    rows: usize,
    owed: usize,
    by_environment: [usize; 3],
    none: usize,
    held: usize,
    statements: usize,
}

/// Everything wrong with the ledger, the list and the ADRs' statements, and what was counted.
/// `adrs` are each ADR's file name and text, `directive` is CLAUDE.md, and `exists` says whether
/// a path is in the tree.
fn problems(
    budgets: &str,
    adrs: &[(String, String)],
    directive: &str,
    exists: &dyn Fn(&str) -> bool,
) -> (Vec<String>, Figure) {
    let mut problems = Vec::new();
    let mut figure = Figure::default();

    // Each ADR's decisions.
    let mut decisions: BTreeMap<u16, BTreeSet<String>> = BTreeMap::new();
    let mut read = Vec::new();
    for (file, text) in adrs {
        match requirements::adr(text) {
            Some(adr) => {
                decisions.insert(
                    adr.number,
                    adr.decisions.iter().map(|d| d.id.clone()).collect(),
                );
                read.push((file.as_str(), adr.number, adr.accepted, text.as_str()));
            }
            None => problems.push(format!("{file}: no `# ADR-NNNN` title, so no ADR is read")),
        }
    }
    let sections = requirements::section_titles(directive);

    // The ledger and the list.
    let Some(tables) = section_tables(budgets) else {
        problems.push(format!("{BUDGETS} has no section `{SECTION}`"));
        return (problems, figure);
    };
    let pick = |columns: &[&str]| -> Vec<&Table> {
        tables
            .iter()
            .filter(|t| {
                t.header
                    .iter()
                    .map(String::as_str)
                    .eq(columns.iter().copied())
            })
            .collect()
    };
    let (ledgers, lists) = (pick(&LEDGER_COLUMNS), pick(&LIST_COLUMNS));
    if ledgers.len() != 1 || lists.len() != 1 {
        problems.push(format!(
            "{BUDGETS}: `{SECTION}` must hold one ledger table (header {LEDGER_COLUMNS:?}) and one list of ADRs (header {LIST_COLUMNS:?}); it holds {} and {}",
            ledgers.len(),
            lists.len()
        ));
        return (problems, figure);
    }
    let (ledger, list) = (ledgers[0], lists[0]);
    if ledger.rows.is_empty() {
        problems.push(format!("{BUDGETS}: the ledger has no row"));
    }

    // Each ledger row: its ID, its cells, what it buys and its figure. `buys` keeps each row's
    // "What the cost buys" cell.
    let mut buys: BTreeMap<String, (usize, String)> = BTreeMap::new();
    for (line, row) in &ledger.rows {
        let at = format!("{BUDGETS}:{line}");
        if row.len() != LEDGER_COLUMNS.len() {
            problems.push(format!(
                "{at}: a ledger row has {} cells, not {}",
                row.len(),
                LEDGER_COLUMNS.len()
            ));
            continue;
        }
        let id = &row[0];
        if !is_ledger_id(id) {
            problems.push(format!(
                "{at}: `{id}` is not a ledger ID (`CL` and a number)"
            ));
            continue;
        }
        if buys.contains_key(id) {
            problems.push(format!("{at}: {id} is used twice"));
            continue;
        }
        for (column, cell) in LEDGER_COLUMNS.iter().zip(row) {
            if cell.is_empty() {
                problems.push(format!("{at}: {id} has no \"{column}\""));
            }
        }
        let bought = &row[2];
        let named = decision_refs(bought);
        let named_sections = section_refs(bought);
        if named.is_empty() && named_sections.is_empty() {
            problems.push(format!(
                "{at}: {id}'s \"What the cost buys\" names no ADR decision (`ADR-NNNN X1`) and no section of the directive (`§N`)"
            ));
        }
        for (number, decision) in &named {
            match decisions.get(number) {
                None => problems.push(format!(
                    "{at}: {id} names ADR-{number:04} {decision}, and there is no ADR-{number:04}"
                )),
                Some(ids) if !ids.contains(decision) => problems.push(format!(
                    "{at}: {id} names ADR-{number:04} {decision}, which is not a decision of ADR-{number:04}"
                )),
                Some(_) => {}
            }
        }
        for section in named_sections {
            if !sections.contains_key(&section) {
                problems.push(format!(
                    "{at}: {id} names §{section}, which the directive does not have"
                ));
            }
        }
        for path in threat_model_refs(bought) {
            if !exists(&path) {
                problems.push(format!("{at}: {id} names {path}, which is not in the tree"));
            }
        }
        let measured = &row[4];
        if measured.starts_with("Unmeasured") {
            figure.owed += 1;
        } else {
            let found: Vec<bool> = ENVIRONMENTS.iter().map(|e| measured.contains(e)).collect();
            if !found.contains(&true) {
                problems.push(format!(
                    "{at}: {id}'s figure names no environment: a \"Measured, and where\" that does not start \"Unmeasured\" names one of `emu`, `virt` and `hw`"
                ));
            }
            for (count, found) in figure.by_environment.iter_mut().zip(found) {
                *count += usize::from(found);
            }
        }
        buys.insert(id.clone(), (*line, bought.clone()));
    }
    figure.rows = buys.len();

    // The list: each ADR from 1 to 17 once, with rows that name it or "none" and a reason.
    let mut listed_adrs: BTreeMap<u16, BTreeSet<String>> = BTreeMap::new();
    let mut listed_rows: BTreeSet<String> = BTreeSet::new();
    for (line, row) in &list.rows {
        let at = format!("{BUDGETS}:{line}");
        if row.len() != LIST_COLUMNS.len() {
            problems.push(format!(
                "{at}: a row of the list has {} cells, not {}",
                row.len(),
                LIST_COLUMNS.len()
            ));
            continue;
        }
        let (who, rows, why) = (&row[0], &row[1], &row[2]);
        let number: Option<u16> = who
            .strip_prefix("ADR-")
            .filter(|n| n.len() == 4)
            .and_then(|n| n.parse().ok());
        let mut named = BTreeSet::new();
        if rows.eq_ignore_ascii_case("none") {
            if why.is_empty() {
                problems.push(format!("{at}: {who} has no ledger row and gives no reason"));
            }
            figure.none += 1;
        } else {
            for id in rows.split(',').map(str::trim) {
                match buys.get(id) {
                    None => problems.push(format!(
                        "{at}: {who} lists `{id}`, which is not a row of the ledger"
                    )),
                    Some((_, bought)) => {
                        if let Some(number) = number
                            && !bought.contains(&format!("ADR-{number:04}"))
                        {
                            problems.push(format!(
                                "{at}: {who} lists {id}, whose \"What the cost buys\" does not name ADR-{number:04}"
                            ));
                        }
                        listed_rows.insert(id.to_string());
                        named.insert(id.to_string());
                    }
                }
            }
        }
        if let Some(number) = number
            && listed_adrs.insert(number, named).is_some()
        {
            problems.push(format!("{at}: ADR-{number:04} is listed twice"));
        }
    }
    for number in LISTED {
        if !listed_adrs.contains_key(&number) {
            problems.push(format!(
                "{BUDGETS}: the list does not name ADR-{number:04}; ADR-0018 V8 asks it to list each of ADR-0001 to ADR-0017, with its rows or with \"none\" and the reason"
            ));
        }
    }
    for (id, (line, bought)) in &buys {
        if !listed_rows.contains(id) {
            problems.push(format!(
                "{BUDGETS}:{line}: {id} is listed under no ADR and not under the threat models"
            ));
        }
        for (number, _) in decision_refs(bought) {
            if LISTED.contains(&number)
                && let Some(rows) = listed_adrs.get(&number)
                && !rows.contains(id)
            {
                problems.push(format!(
                    "{BUDGETS}:{line}: {id} names ADR-{number:04}, and the list does not list it under ADR-{number:04}"
                ));
            }
        }
    }

    // The statements of every part accepted after ADR-0018.
    for (file, number, accepted, text) in read {
        for held in held_parts(number, accepted, text) {
            figure.held += 1;
            let at = format!("docs/adr/{file}:{} ({})", held.line, held.part);
            if held.statements.is_empty() {
                problems.push(format!(
                    "{at}: accepted after ADR-0018 and states no performance cost; ADR-0018 V8 asks for a \"Performance cost\" statement in one of V3's forms"
                ));
            }
            for (line, said) in &held.statements {
                figure.statements += 1;
                let at = format!("docs/adr/{file}:{line}");
                let forms = forms(said);
                if forms == Forms::default() {
                    problems.push(format!(
                        "{at}: the cost is stated in none of ADR-0018 V3's forms (\"None, by construction\", \"Measured\", \"Unmeasured, owed\")"
                    ));
                }
                if (forms.owed || forms.measured) && ledger_ids(said).is_empty() {
                    problems.push(format!(
                        "{at}: a cost stated \"Measured\" or \"Unmeasured, owed\" names no ledger row (`CL` and a number); ADR-0018 V8"
                    ));
                }
            }
        }
        // Every ledger ID the ADR names, wherever it is, is a row of the ledger.
        for (index, line) in text.lines().enumerate() {
            for id in ledger_ids(line) {
                if !buys.contains_key(&id) {
                    problems.push(format!(
                        "docs/adr/{file}:{}: names {id}, which the ledger does not hold",
                        index + 1
                    ));
                }
            }
        }
    }
    (problems, figure)
}

/// The gate: the ledger and its list are whole and true to the ADRs, and every ADR or amendment
/// accepted after ADR-0018 states its cost.
pub(crate) fn gate() -> Result {
    let read = requirements::read()?;
    let path = repo_root().join(BUDGETS);
    let budgets = fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let root = repo_root();
    let (problems, figure) = problems(&budgets, &read.adrs, &read.directive, &|p: &str| {
        root.join(p).exists()
    });
    if !problems.is_empty() {
        return Err(format!(
            "the cost ledger in {BUDGETS} or an ADR's cost is not as ADR-0018 V5 and V8 ask (its rules are in developer-tools/xtask/src/costs.rs):\n  {}",
            problems.join("\n  ")
        ));
    }
    let [emu, virt, hw] = figure.by_environment;
    println!(
        "xtask: cost gate: {} ledger rows, {} owed and {} with a figure (`emu` {emu}, `virt` {virt}, `hw` {hw}); ADR-0001 to ADR-0017 listed, {} with none; {} ADRs or amendments accepted after ADR-0018, {} statements of cost, each in V3's forms",
        figure.rows,
        figure.owed,
        figure.rows - figure.owed,
        figure.none,
        figure.held,
        figure.statements
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A directive with sections 14 and 30.
    const DIRECTIVE: &str = "# Directive\n\n## 14. Compiler and linker security\n\nText.\n\n## 30. IPC and capabilities\n\nText.\n";

    /// ADR-0005 with eight amendments from before the policy, the last with no statement, and a
    /// ninth that states its cost.
    fn adr5(ninth: &str) -> String {
        let mut text = String::from(
            "# ADR-0005: Processes\n\n| | |\n|---|---|\n| Status | **Accepted** |\n\n### P4. The system-call convention\n\nText.\n\n",
        );
        for n in 1..=8 {
            text.push_str(&format!(
                "## Amendment {n} (Accepted, 2026-11-0{n}): old\n\nText.\n\n"
            ));
        }
        text.push_str(&format!(
            "## Amendment 9 (Accepted, 2026-11-13): the flags\n\n| # | Decision |\n|---|---|\n| H1 | **Flags.** Text. |\n\n{ninth}\n"
        ));
        text
    }

    /// ADR-0017 with Z3 and Z9, and its one amendment from before the policy.
    const ADR17: &str = "# ADR-0017: Speculation\n\n| | |\n|---|---|\n| Status | **Accepted** |\n\n### Z3. IBPB between programs\n\nText.\n\n### Z9. One primitive\n\nText.\n\n### Amendment 1: libraries (item 152)\n\nText.\n";

    /// The ledger's header.
    fn ledger_header() -> String {
        format!(
            "| {} |\n|---|---|---|---|---|---|---|\n",
            LEDGER_COLUMNS.join(" | ")
        )
    }

    /// A budgets file: the ledger's `rows`, and a list with `listed` and "none" for every other
    /// ADR from 1 to 17.
    fn budgets(rows: &str, listed: &[(u16, &str)]) -> String {
        let mut text = format!(
            "# Budgets\n\n## The budgets\n\n| A | B |\n|---|---|\n| x | y |\n\n{SECTION}\n\nText.\n\n{}{rows}\nText.\n\n### Each ADR\n\n| ADR | Rows | Why none |\n|---|---|---|\n",
            ledger_header()
        );
        for number in LISTED {
            match listed.iter().find(|(n, _)| *n == number) {
                Some((_, rows)) => text.push_str(&format!("| ADR-{number:04} | {rows} | |\n")),
                None => text.push_str(&format!("| ADR-{number:04} | none | No cost. |\n")),
            }
        }
        text.push_str("\n## Adding a figure\n\nText.\n");
        text
    }

    /// Two ledger rows: one measured, one owed.
    const ROWS: &str = "| CL1 | A lookup | ADR-0017 Z9 | A mask | `virt` 1.2 ns | LFENCE | Intel |\n| CL2 | IBPB | ADR-0017 Z3; CLAUDE.md §14; `THREAT_MODEL_SYSCALLS.md` E5 | One barrier | Unmeasured, owed | Trust classes | A figure |\n";

    /// ADR-0005's ninth amendment, stated as owed and naming CL2.
    const OWED: &str = "**Performance cost (ADR-0018 V3).** H1: Unmeasured, owed (ledger row CL2).";

    fn run(budgets: &str, adr5_text: &str, more: &[(&str, &str)]) -> Vec<String> {
        let mut adrs = vec![
            ("ADR-0005-process.md".to_string(), adr5_text.to_string()),
            ("ADR-0017-speculation.md".to_string(), ADR17.to_string()),
        ];
        for (name, text) in more {
            adrs.push(((*name).to_string(), (*text).to_string()));
        }
        let exists = |path: &str| path == "docs/engineering/THREAT_MODEL_SYSCALLS.md";
        problems(budgets, &adrs, DIRECTIVE, &exists).0
    }

    fn good() -> String {
        budgets(ROWS, &[(17, "CL1, CL2")])
    }

    fn refused(problems: &[String], want: &str) {
        assert!(
            problems.iter().any(|p| p.contains(want)),
            "expected a problem containing {want:?}, got {problems:#?}"
        );
    }

    #[test]
    fn the_fixture_passes() {
        assert_eq!(run(&good(), &adr5(OWED), &[]), Vec::<String>::new());
    }

    #[test]
    fn an_amendment_after_the_policy_with_no_statement_is_refused() {
        refused(
            &run(&good(), &adr5(""), &[]),
            "(Amendment 9): accepted after ADR-0018 and states no performance cost",
        );
    }

    #[test]
    fn an_amendment_from_before_the_policy_is_not_held() {
        // Amendment 8 has no statement and is not refused: the fixture passes.
        let problems = run(&good(), &adr5(OWED), &[]);
        assert!(!problems.iter().any(|p| p.contains("Amendment 8")));
    }

    #[test]
    fn a_proposed_amendment_is_not_held() {
        let text = adr5(OWED).replace(
            "## Amendment 9 (Accepted, 2026-11-13)",
            "## Amendment 9 (Proposed, 2026-11-13)",
        );
        let text = text.replace(OWED, "");
        assert_eq!(run(&good(), &text, &[]), Vec::<String>::new());
    }

    #[test]
    fn a_statement_in_no_form_is_refused() {
        let said = "**Performance cost (ADR-0018 V3).** Cheap.";
        refused(
            &run(&good(), &adr5(said), &[]),
            "none of ADR-0018 V3's forms",
        );
    }

    #[test]
    fn unmeasured_and_owed_counts_as_the_owed_form() {
        let said = "**Performance cost (ADR-0018 V3).** H1: one byte. Unmeasured and owed (CL2).";
        assert_eq!(run(&good(), &adr5(said), &[]), Vec::<String>::new());
    }

    #[test]
    fn none_by_construction_needs_no_row() {
        let said = "**Performance cost (ADR-0018 V3).** None, by construction: it states what the kernel does.";
        assert_eq!(run(&good(), &adr5(said), &[]), Vec::<String>::new());
    }

    #[test]
    fn an_owed_cost_that_names_no_row_is_refused() {
        let said = "**Performance cost (ADR-0018 V3).** H1: Unmeasured, owed: S4-2 measures it.";
        refused(&run(&good(), &adr5(said), &[]), "names no ledger row");
    }

    #[test]
    fn a_measured_cost_that_names_no_row_is_refused() {
        let said = "| Performance cost | Measured: 3 ns in `virt` |";
        refused(&run(&good(), &adr5(said), &[]), "names no ledger row");
    }

    #[test]
    fn a_row_the_ledger_lacks_is_refused() {
        let said = "**Performance cost (ADR-0018 V3).** Unmeasured, owed (CL9).";
        refused(
            &run(&good(), &adr5(said), &[]),
            "names CL9, which the ledger does not hold",
        );
    }

    #[test]
    fn a_ledger_id_anywhere_in_an_adr_must_be_a_row() {
        // ADR-0017's text, outside any part held to state a cost, names CL2 and then CL7.
        let named = ADR17.replace(
            "### Z9. One primitive\n\nText.",
            "### Z9. One primitive\n\nIts cost is cost ledger row CL2.",
        );
        let adrs = |text: &str| {
            vec![
                ("ADR-0005-process.md".to_string(), adr5(OWED)),
                ("ADR-0017-speculation.md".to_string(), text.to_string()),
            ]
        };
        let exists = |path: &str| path == "docs/engineering/THREAT_MODEL_SYSCALLS.md";
        assert_eq!(
            problems(&good(), &adrs(&named), DIRECTIVE, &exists).0,
            Vec::<String>::new()
        );
        let missing = named.replace("row CL2.", "row CL7.");
        refused(
            &problems(&good(), &adrs(&missing), DIRECTIVE, &exists).0,
            "ADR-0017-speculation.md:13: names CL7, which the ledger does not hold",
        );
    }

    #[test]
    fn a_new_adr_is_held_once_accepted() {
        let accepted = "# ADR-0019: New\n\n| | |\n|---|---|\n| Status | **Accepted** |\n\n### N1. A rule\n\nText.\n";
        refused(
            &run(&good(), &adr5(OWED), &[("ADR-0019-new.md", accepted)]),
            "ADR-0019-new.md:1 (the ADR): accepted after ADR-0018 and states no performance cost",
        );
        let proposed = accepted.replace("**Accepted**", "**Proposed**");
        assert_eq!(
            run(&good(), &adr5(OWED), &[("ADR-0019-new.md", &proposed)]),
            Vec::<String>::new()
        );
        let stated = format!("{accepted}\n| Performance cost | None, by construction: a rule. |\n");
        assert_eq!(
            run(&good(), &adr5(OWED), &[("ADR-0019-new.md", &stated)]),
            Vec::<String>::new()
        );
    }

    #[test]
    fn a_row_whose_decision_does_not_exist_is_refused() {
        let rows = ROWS.replace("ADR-0017 Z9", "ADR-0017 Z99");
        refused(
            &run(&budgets(&rows, &[(17, "CL1, CL2")]), &adr5(OWED), &[]),
            "names ADR-0017 Z99, which is not a decision of ADR-0017",
        );
    }

    #[test]
    fn a_row_naming_a_missing_adr_section_or_threat_model_is_refused() {
        let rows = ROWS
            .replace("ADR-0017 Z3;", "ADR-0017 Z3; ADR-0042 X1;")
            .replace("§14", "§99")
            .replace("THREAT_MODEL_SYSCALLS", "THREAT_MODEL_NOWHERE");
        let problems = run(&budgets(&rows, &[(17, "CL1, CL2")]), &adr5(OWED), &[]);
        refused(&problems, "there is no ADR-0042");
        refused(&problems, "names §99");
        refused(
            &problems,
            "THREAT_MODEL_NOWHERE.md, which is not in the tree",
        );
    }

    #[test]
    fn a_row_that_names_nothing_it_buys_is_refused() {
        let rows = ROWS.replace("| ADR-0017 Z9 |", "| a protection |");
        refused(
            &run(&budgets(&rows, &[(17, "CL1, CL2")]), &adr5(OWED), &[]),
            "names no ADR decision",
        );
    }

    #[test]
    fn a_figure_with_no_environment_is_refused() {
        let rows = ROWS.replace("`virt` 1.2 ns", "1.2 ns");
        refused(
            &run(&budgets(&rows, &[(17, "CL1, CL2")]), &adr5(OWED), &[]),
            "CL1's figure names no environment",
        );
    }

    #[test]
    fn the_list_must_name_each_adr_to_0017_once() {
        let text = good().replace("| ADR-0007 | none | No cost. |\n", "");
        refused(&run(&text, &adr5(OWED), &[]), "does not name ADR-0007");
        let text = good().replace(
            "| ADR-0007 | none | No cost. |\n",
            "| ADR-0007 | none | No cost. |\n| ADR-0007 | none | Again. |\n",
        );
        refused(&run(&text, &adr5(OWED), &[]), "ADR-0007 is listed twice");
    }

    #[test]
    fn none_needs_a_reason() {
        let text = good().replace("| ADR-0007 | none | No cost. |", "| ADR-0007 | none | |");
        refused(
            &run(&text, &adr5(OWED), &[]),
            "ADR-0007 has no ledger row and gives no reason",
        );
    }

    #[test]
    fn the_list_and_the_rows_agree() {
        // A row listed under an ADR it does not name.
        let text = budgets(ROWS, &[(17, "CL1, CL2"), (3, "CL1")]);
        refused(
            &run(&text, &adr5(OWED), &[]),
            "lists CL1, whose \"What the cost buys\" does not name ADR-0003",
        );
        // A row the list leaves out.
        let text = budgets(ROWS, &[(17, "CL1")]);
        let problems = run(&text, &adr5(OWED), &[]);
        refused(&problems, "CL2 is listed under no ADR");
        refused(
            &problems,
            "CL2 names ADR-0017, and the list does not list it under ADR-0017",
        );
        // A row the ledger does not hold.
        let text = budgets(ROWS, &[(17, "CL1, CL2, CL3")]);
        refused(
            &run(&text, &adr5(OWED), &[]),
            "lists `CL3`, which is not a row of the ledger",
        );
    }

    #[test]
    fn ids_are_checked() {
        let rows = ROWS.replace("| CL2 |", "| CL1 |");
        refused(
            &run(&budgets(&rows, &[(17, "CL1")]), &adr5(""), &[]),
            "CL1 is used twice",
        );
        let rows = ROWS.replace("| CL2 |", "| CL02 |");
        refused(
            &run(&budgets(&rows, &[(17, "CL1")]), &adr5(""), &[]),
            "`CL02` is not a ledger ID",
        );
    }

    #[test]
    fn an_empty_cell_is_refused() {
        let rows = ROWS.replace("| LFENCE |", "| |");
        refused(
            &run(&budgets(&rows, &[(17, "CL1, CL2")]), &adr5(OWED), &[]),
            "CL1 has no \"Cheaper design considered\"",
        );
    }

    #[test]
    fn a_missing_section_or_table_fails_closed() {
        let text = good().replace(SECTION, "## Another section");
        refused(&run(&text, &adr5(OWED), &[]), "has no section");
        let text = good().replace("| Row | Path |", "| Line | Path |");
        refused(&run(&text, &adr5(OWED), &[]), "must hold one ledger table");
        let text = budgets("", &[]);
        refused(&run(&text, &adr5(OWED), &[]), "the ledger has no row");
    }

    #[test]
    fn the_readers_find_what_they_should() {
        assert_eq!(
            ledger_ids("CL1, CL22; XCL3 CL4a (CL5)"),
            vec!["CL1", "CL22", "CL5"]
        );
        assert_eq!(
            decision_refs("ADR-0017 Z9; ADR-0001 def5, ADR-0010 Am1 and ADR-0014 Amendment 1"),
            vec![
                (17, "Z9".to_string()),
                (1, "def5".to_string()),
                (10, "Am1".to_string())
            ]
        );
        assert_eq!(section_refs("CLAUDE.md §14 and §30."), vec![14, 30]);
        assert_eq!(forms("Unmeasured"), Forms::default());
        assert!(forms("Measured: 3 ns").measured);
        assert_eq!(
            statement("**Performance cost (ADR-0018 V3).** None, by construction."),
            Some("None, by construction.".to_string())
        );
        assert_eq!(
            statement("| Performance cost | None, by construction: x |"),
            Some("None, by construction: x".to_string())
        );
        assert_eq!(statement("Performance cost is low."), None);
    }

    /// The checkout's ledger and ADRs pass.
    #[test]
    fn the_checkout_passes() {
        gate().expect("the cost gate on this checkout");
    }
}
