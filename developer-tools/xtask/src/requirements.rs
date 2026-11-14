//! `cargo xtask check`'s gate on the requirements matrix (CLAUDE.md §1; TASKS item 35; decision
//! items 191 to 196).
//!
//! CLAUDE.md §1 makes 100% coverage of approved requirements the completion target, "measured
//! against a versioned requirements matrix". The matrix is the Markdown tables under
//! `docs/requirements/`: they are the only copy, and that directory's `README.md` holds their
//! rules. This gate keeps the matrix complete and true to the tree, and prints the coverage
//! figure.
//!
//! **What a row is** (item 191): one for each statement of CLAUDE.md and one for each decision
//! of an accepted ADR. Both are found by rule ([`directive`], [`adr`]), and a row quotes what it
//! stands for, so the gate holds the matrix to its sources in both directions: a statement with
//! no row fails, and so does a row whose statement is gone.
//!
//! **What it refuses:**
//!
//! - a file of the matrix that does not parse, a row with no owner or an owner that is not one
//!   of §1's roles, an ID used twice, a row in the wrong file or out of its source's order;
//! - a statement or a decision with no row, and a row that quotes what its source no longer says;
//! - a state the row's other cells do not support: evidence or an implementation that is not in
//!   the tree, `done` on evidence from a weaker environment than the row's scope (item 193),
//!   anything past `not-started` without acceptance criteria (item 195), a `removed` row that
//!   names no decision item or that no scope change names (item 196);
//! - an answered decision item that no row names and the README does not list, and a row or a
//!   scope change that names an item the decision sheet does not hold as answered;
//! - a row that says its evidence ran on hardware and names no record the gate can read
//!   ([`RECORD_KEYS`]);
//! - a heading, a code block or a table's header of the directive that is not as its PART's
//!   recorded digest ([`unquoted`]): with the rows' quotes, every line of the directive is held;
//! - a row that is `done` or `info` without its ID in the README's lists, so the figure cannot
//!   move by one cell alone;
//! - a matrix checked against another revision of the directive than the one in the tree;
//! - a run that read no row: nothing was checked.
//!
//! **What it does not see:** whether a row's owner, scope, kind, acceptance criteria or state is
//! the right judgement; whether evidence that exists passes, which is `cargo xtask check` and the
//! boots; where evidence ran, beyond asking a hardware record of a row that says hardware; who
//! reviewed a row; whether a row's acceptance criterion is its own, which it prints a count of;
//! and a decision of an ADR whose body changes under an unchanged title.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::fs;
use std::path::PathBuf;

use crate::{Result, repo_root};

/// The matrix's directory.
const MATRIX: &str = "docs/requirements";
/// The directive, whose statements are rows.
const DIRECTIVE: &str = "CLAUDE.md";
/// The ADRs, whose decisions are rows once the ADR is accepted.
const ADRS: &str = "docs/adr";
/// The decision sheet, whose answered items the rows name.
const SHEET: &str = "docs/engineering/OWNER_DECISIONS.md";
/// Where a boot proof's name is looked for: `proof:<name>`.
const PROOFS: &str = "framekernel/src/bootproof.rs";
/// Where a `cargo xtask` command is looked for: `xtask:<command>`.
const COMMANDS: &str = "developer-tools/xtask/src/main.rs";
/// Where the record of a run on a physical machine is kept (CLAUDE.md §74, §79). Other files
/// under `docs/hardware/`, the shortlist of machines to buy among them, are no such record.
const HARDWARE_RECORDS: &str = "docs/hardware/records/";
/// What a hardware record says, each as a row of a table in it.
const RECORD_KEYS: [&str; 5] = ["Machine", "Firmware", "Source revision", "Date", "Result"];

/// The header of every table of the matrix.
const COLUMNS: [&str; 13] = [
    "ID",
    "Requirement",
    "Kind",
    "Scope",
    "Owner",
    "State",
    "Implementation",
    "Acceptance",
    "Evidence",
    "Env",
    "Review",
    "Limitations",
    "Decided by",
];
/// The header of the README's table of approved scope changes (item 196).
const CHANGES: [&str; 4] = ["Item", "Date", "What changed", "Rows"];
/// The header of the README's table of answered decision items that no row names (item 191).
const UNROWED: [&str; 3] = ["Item", "Rows", "Why"];
/// The header of the README's table of what the rows do not quote: each PART's headings, code
/// blocks and table headers, as one digest.
const UNQUOTED: [&str; 2] = ["Part", "Headings and code blocks"];
/// The header of the README's lists of the rows that move the figure: `done` and `info`.
const LISTS: [&str; 2] = ["List", "Rows"];
/// The first cell of the README's row that says which revision of the directive the matrix
/// was checked against.
const REVISION_KEY: &str = "Directive revision";
/// How many problems of one kind are listed before the rest are only counted.
const LISTED: usize = 25;

// ---------------------------------------------------------------------------------------------
// Markdown tables
// ---------------------------------------------------------------------------------------------

/// A Markdown table.
#[derive(Debug)]
struct Table {
    /// Its header's cells.
    header: Vec<String>,
    /// Each body row: its line number, from 1, and its cells.
    rows: Vec<(usize, Vec<String>)>,
}

/// The cells of one table line: the text between its `|`s, trimmed, with `\|` read as `|`.
/// `None` if the line does not start and end with a `|`.
fn cells(line: &str) -> Option<Vec<String>> {
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

/// `text` as it is written inside a table cell: the inverse of [`cells`].
fn escaped(text: &str) -> String {
    text.replace('|', "\\|")
}

/// The tables of a Markdown text, outside fenced code. A line that starts with `|` and is not a
/// table line is kept as a row of one cell, so a reader that counts cells refuses it.
fn tables(text: &str) -> Vec<Table> {
    let mut tables: Vec<Table> = Vec::new();
    let mut open = false;
    let mut fenced = false;
    for (index, line) in text.lines().enumerate() {
        if line.trim_start().starts_with("```") {
            fenced = !fenced;
            open = false;
            continue;
        }
        if fenced || !line.trim_start().starts_with('|') {
            open = false;
            continue;
        }
        let cells = cells(line).unwrap_or_else(|| vec![line.to_string()]);
        match tables.last_mut() {
            Some(table) if open => {
                let rule = cells
                    .iter()
                    .all(|c| !c.is_empty() && c.chars().all(|ch| matches!(ch, '-' | ':')));
                if !(rule && table.rows.is_empty()) {
                    table.rows.push((index + 1, cells));
                }
            }
            _ => {
                tables.push(Table {
                    header: cells,
                    rows: Vec::new(),
                });
                open = true;
            }
        }
    }
    tables
}

/// Whether `header` is exactly the cells `want`.
fn is_header(header: &[String], want: &[&str]) -> bool {
    header.len() == want.len() && header.iter().zip(want).all(|(have, want)| have == want)
}

/// The decision items a cell names: `-` for none, or numbers and ranges such as `71–76, 88`.
/// `None` if the cell is anything else.
fn items(cell: &str) -> Option<Vec<u32>> {
    let cell = cell.trim();
    if cell == "-" {
        return Some(Vec::new());
    }
    let mut items = Vec::new();
    for part in cell.split(',') {
        let part = part.trim();
        let (first, last) = match part.split_once(['–', '-']) {
            Some((first, last)) => (first.trim(), last.trim()),
            None => (part, part),
        };
        let number = |text: &str| {
            (!text.is_empty() && text.bytes().all(|b| b.is_ascii_digit()))
                .then(|| text.parse::<u32>().ok())
                .flatten()
        };
        let (first, last) = (number(first)?, number(last)?);
        if first > last || last - first > 999 {
            return None;
        }
        items.extend(first..=last);
    }
    Some(items)
}

// ---------------------------------------------------------------------------------------------
// The directive's statements
// ---------------------------------------------------------------------------------------------

/// Where in CLAUDE.md a statement is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Section {
    /// Before PART I: the title block and the table of formats.
    Header,
    /// Between a PART's heading and its first numbered section.
    Part(u8),
    /// A numbered section, such as `## 33. NVMe`.
    Numbered(u8),
    /// The final mandatory rules.
    Final,
}

impl Section {
    /// What the IDs of its rows start with: `H`, `P9`, `S33`, `F`.
    fn prefix(self) -> String {
        match self {
            Self::Header => "H".to_string(),
            Self::Part(part) => format!("P{part}"),
            Self::Numbered(section) => format!("S{section}"),
            Self::Final => "F".to_string(),
        }
    }

    /// Its name in a message.
    fn named(self) -> String {
        match self {
            Self::Header => "the header".to_string(),
            Self::Part(part) => format!("the opening of PART {part}"),
            Self::Numbered(section) => format!("§{section}"),
            Self::Final => "the final rules".to_string(),
        }
    }
}

/// One statement of the directive.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Statement {
    section: Section,
    /// Which PART it is in: 0 for the header, and one past the last PART for the final rules.
    part: u8,
    /// The line it starts on, from 1.
    line: usize,
    /// Its text, with the lead-in it stands under joined in front.
    text: String,
}

/// What the gate reads from CLAUDE.md.
#[derive(Debug, Default)]
struct Directive {
    /// The revision its header gives.
    revision: Option<String>,
    statements: Vec<Statement>,
    /// The numbers of §1's roles: what a row's owner may be.
    roles: BTreeSet<u8>,
    /// Lines the statement rule does not define. Each fails the gate.
    problems: Vec<String>,
    /// For each PART, the SHA-256 of the lines no row quotes: see [`unquoted`].
    unquoted: BTreeMap<u8, String>,
    /// Each numbered section's title, as its heading `## N. Title` gives it.
    titles: BTreeMap<u8, String>,
}

/// Bytes as lower-case hexadecimal.
fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::new(), |mut out, byte| {
        let _ = write!(out, "{byte:02x}");
        out
    })
}

/// What no row of the matrix quotes, as one digest for each PART of the directive (0 for the
/// header, one past the last PART for the final rules): its headings, its rules, its fenced code
/// with the fences, and its tables' header and rule rows. With the statements, that is every
/// line of the directive that is not blank.
fn unquoted(text: &str) -> BTreeMap<u8, String> {
    let mut lines: BTreeMap<u8, Vec<&str>> = BTreeMap::new();
    let mut part = 0u8;
    let mut fenced = false;
    let mut in_table = false;
    for raw in text.lines() {
        let line = raw.trim_end();
        let fence = raw.starts_with("```");
        if fence {
            fenced = !fenced;
        }
        let kept = if fence || fenced {
            in_table = false;
            true
        } else if line.starts_with('#') || line == "---" {
            in_table = false;
            if line.starts_with("# PART ") || line.starts_with("# FINAL MANDATORY") {
                part += 1;
            }
            true
        } else if line.starts_with('|') {
            let rule = cells(line).is_some_and(|cells| {
                cells
                    .iter()
                    .all(|c| !c.is_empty() && c.chars().all(|ch| matches!(ch, '-' | ':')))
            });
            let header = !in_table;
            in_table = true;
            header || rule
        } else {
            in_table = false;
            false
        };
        if kept {
            lines.entry(part).or_default().push(line);
        } else {
            lines.entry(part).or_default();
        }
    }
    lines
        .into_iter()
        .map(|(part, lines)| {
            (
                part,
                hex(&origin_crypto::Sha256::digest(lines.join("\n").as_bytes())),
            )
        })
        .collect()
}

/// A piece of the directive, before lead-ins are joined.
#[derive(Debug)]
enum Block {
    Paragraph(String),
    /// A list item: its text, with the number of a numbered item kept.
    Item(String),
    /// A table's body rows, each as its cells joined with `: `.
    Rows(Vec<(usize, String)>),
    /// A heading, a rule or a code block: nothing joins across it.
    Break,
}

/// Whether `line` starts a numbered list item: digits, a full stop and a space.
fn numbered(line: &str) -> bool {
    let digits = line.bytes().take_while(u8::is_ascii_digit).count();
    digits > 0 && line[digits..].starts_with(". ")
}

/// Whether a paragraph is only a label, or ends in one after a lead-in: `**Origin userspace**`,
/// `Use: **UEFI bootloader**`. A bold sentence, which ends `.**`, is not one.
fn is_label(text: &str) -> bool {
    let Some(inner) = text.strip_suffix("**") else {
        return false;
    };
    if inner.ends_with('.') {
        return false;
    }
    let Some(start) = inner.rfind("**") else {
        return false;
    };
    let label = &inner[start + 2..];
    !label.is_empty() && !label.contains('*') && (start == 0 || inner[..start].ends_with(": "))
}

/// The statements of the directive.
///
/// A statement is, outside fenced code:
///
/// - a line of the header that starts with a bold label;
/// - a body row of a table;
/// - a list item, bulleted or numbered (a numbered item keeps its number);
/// - a paragraph that is not a lead-in.
///
/// A lead-in is a paragraph that ends with a colon. It is joined in front of each item of the
/// list after it, or of the one paragraph after it. A paragraph that is only a bold label is
/// joined in front of the paragraph after it, unless that is a label too. A lead-in with a code
/// block after it is a statement by itself.
fn directive(text: &str) -> Directive {
    let mut out = Directive {
        unquoted: unquoted(text),
        ..Directive::default()
    };
    let mut blocks: Vec<(Section, u8, usize, Block)> = Vec::new();
    let mut section = Section::Header;
    let mut part = 0u8;
    let mut fenced = false;
    // Whether the last block is a paragraph or a table a following line may add to.
    let mut open = false;
    for (index, raw) in text.lines().enumerate() {
        let line_number = index + 1;
        if raw.starts_with("```") {
            fenced = !fenced;
            open = false;
            if fenced {
                blocks.push((section, part, line_number, Block::Break));
            }
            continue;
        }
        if fenced {
            continue;
        }
        let line = raw.trim_end();
        if line.is_empty() {
            open = false;
            continue;
        }
        if line.starts_with('#') || line == "---" {
            open = false;
            if line.starts_with("# PART ") {
                part += 1;
                section = Section::Part(part);
            } else if line.starts_with("# FINAL MANDATORY") {
                part += 1;
                section = Section::Final;
            } else if let Some(rest) = line.strip_prefix("## ") {
                let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
                if digits > 0 && rest[digits..].starts_with(". ") {
                    match rest[..digits].parse() {
                        Ok(number) => {
                            section = Section::Numbered(number);
                            out.titles
                                .insert(number, rest[digits + 2..].trim().to_string());
                        }
                        Err(_) => out.problems.push(format!(
                            "line {line_number}: a section number too large to read"
                        )),
                    }
                }
            }
            blocks.push((section, part, line_number, Block::Break));
            continue;
        }
        if line.starts_with([' ', '\t']) {
            out.problems.push(format!(
                "line {line_number} is indented: a continuation line or a nested item, which the statement rule does not define"
            ));
            open = false;
            continue;
        }
        if let Some(item) = line.strip_prefix("- ") {
            open = false;
            blocks.push((section, part, line_number, Block::Item(item.to_string())));
        } else if numbered(line) {
            open = false;
            blocks.push((section, part, line_number, Block::Item(line.to_string())));
        } else if line.starts_with('|') {
            let Some(cells) = cells(line) else {
                out.problems.push(format!(
                    "line {line_number} starts a table row and does not end one"
                ));
                open = false;
                continue;
            };
            let rule = cells
                .iter()
                .all(|c| !c.is_empty() && c.chars().all(|ch| matches!(ch, '-' | ':')));
            match blocks.last_mut() {
                Some((_, _, _, Block::Rows(rows))) if open => {
                    if !rule {
                        rows.push((line_number, cells.join(": ")));
                    }
                }
                // The first line of a table is its header, which is no statement.
                _ => {
                    blocks.push((section, part, line_number, Block::Rows(Vec::new())));
                    open = true;
                }
            }
        } else {
            let bold_header_line = section == Section::Header && line.starts_with("**");
            match blocks.last_mut() {
                Some((_, _, _, Block::Paragraph(paragraph))) if open && !bold_header_line => {
                    paragraph.push(' ');
                    paragraph.push_str(line);
                }
                _ => {
                    blocks.push((
                        section,
                        part,
                        line_number,
                        Block::Paragraph(line.to_string()),
                    ));
                    open = !bold_header_line;
                }
            }
        }
    }

    // A lead-in waiting for the items of its list.
    let mut lead: Option<String> = None;
    // A lead-in or a label waiting for its paragraph, with the line it started on.
    let mut carried: Option<(usize, String)> = None;
    for index in 0..blocks.len() {
        let (section, part, line, block) = &blocks[index];
        let next = blocks
            .get(index + 1)
            .filter(|(next_section, ..)| next_section == section)
            .map(|(.., block)| block);
        let mut state = |line: usize, text: String| {
            if *section == Section::Header
                && let Some(rest) = text.strip_prefix("**Revision:** ")
            {
                out.revision = rest.split(',').next().map(|r| r.trim().to_string());
            }
            if *section == Section::Numbered(1) && numbered(&text) {
                let digits = text.bytes().take_while(u8::is_ascii_digit).count();
                if text[digits + 2..].starts_with("**")
                    && let Ok(role) = text[..digits].parse()
                {
                    out.roles.insert(role);
                }
            }
            out.statements.push(Statement {
                section: *section,
                part: *part,
                line,
                text,
            });
        };
        match block {
            Block::Break => {
                lead = None;
                carried = None;
            }
            Block::Rows(rows) => {
                lead = None;
                carried = None;
                for (line, text) in rows {
                    state(*line, text.clone());
                }
            }
            Block::Item(item) => {
                carried = None;
                let text = match &lead {
                    Some(lead) => format!("{lead} {item}"),
                    None => item.clone(),
                };
                state(*line, text);
            }
            Block::Paragraph(paragraph) => {
                let (line, text) = match carried.take() {
                    Some((line, before)) => (line, format!("{before} {paragraph}")),
                    None => (*line, paragraph.clone()),
                };
                if text.ends_with(':') && matches!(next, Some(Block::Item(_))) {
                    lead = Some(text);
                    continue;
                }
                lead = None;
                let joins = match next {
                    Some(Block::Paragraph(after)) => {
                        text.ends_with(':') || (is_label(&text) && !is_label(after))
                    }
                    _ => false,
                };
                if joins {
                    carried = Some((line, text));
                } else {
                    state(line, text);
                }
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------------------------
// The ADRs' decisions
// ---------------------------------------------------------------------------------------------

/// One decision of an ADR.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Decision {
    /// Its identifier as a row's ID ends: `P3`, `D1`, `def4`, `Am1`.
    pub(crate) id: String,
    /// What a row quotes for it.
    title: String,
    /// The line it is on, from 1.
    line: usize,
}

/// What the gate reads from one ADR.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Adr {
    pub(crate) number: u16,
    /// What its `# ADR-NNNN: Title` heading says after the colon.
    title: String,
    /// Whether its status is Accepted: only then are its decisions requirements.
    pub(crate) accepted: bool,
    pub(crate) decisions: Vec<Decision>,
}

/// Whether `text` is a decision's identifier: one capital letter and a number.
fn is_decision_id(text: &str) -> bool {
    let mut bytes = text.bytes();
    bytes.next().is_some_and(|b| b.is_ascii_uppercase())
        && text.len() > 1
        && bytes.all(|b| b.is_ascii_digit())
}

/// `X1. Title` split into its identifier and its title.
fn id_and_title(text: &str) -> Option<(&str, &str)> {
    let (id, title) = text.split_once(". ")?;
    is_decision_id(id).then_some((id, title.trim()))
}

/// What a row quotes for a decision that is a table row: its leading bold phrase, or else its
/// first sentence, without the punctuation that ends either.
fn title_of(cell: &str) -> String {
    let title = match cell
        .strip_prefix("**")
        .and_then(|rest| rest.split_once("**"))
    {
        Some((bold, _)) => bold,
        None => cell.split_once(". ").map_or(cell, |(first, _)| first),
    };
    title.trim().trim_end_matches(['.', ',', ':']).to_string()
}

/// The decisions of an ADR, or `None` if the text has no `# ADR-NNNN` title.
///
/// A decision is:
///
/// - a heading `## Decision N: title`;
/// - a heading `### X1. title`;
/// - a paragraph that starts `**X1. title.**`, under a second-level heading that starts with
///   "Decisions" or "Proposed decisions";
/// - a row `| X1 | … |` of a table whose header starts with `#` and has a "Decision" column: it
///   is quoted by its "Question" cell if the table has one, and by [`title_of`] its decision
///   otherwise;
/// - a numbered item under "Approved defaults";
/// - an amendment that holds none of these: it is one decision, quoted by its heading after the
///   colon.
pub(crate) fn adr(text: &str) -> Option<Adr> {
    /// An amendment whose end has not been reached.
    struct Amendment {
        number: String,
        level: usize,
        title: String,
        line: usize,
        found: usize,
    }
    /// The columns of the table being read, if it holds decisions.
    struct Columns {
        decision: usize,
        question: Option<usize>,
    }

    let mut number = None;
    let mut title = String::new();
    let mut accepted = false;
    let mut decisions: Vec<Decision> = Vec::new();
    let mut second = String::new();
    let mut amendment: Option<Amendment> = None;
    let mut columns: Option<Columns> = None;
    let mut in_table = false;
    let mut fenced = false;

    /// Ends an amendment: one that held no decision is one itself.
    fn close(amendment: &mut Option<Amendment>, decisions: &mut Vec<Decision>) {
        if let Some(amendment) = amendment.take()
            && amendment.found == 0
        {
            decisions.push(Decision {
                id: format!("Am{}", amendment.number),
                title: amendment.title,
                line: amendment.line,
            });
        }
    }
    /// Records a decision, in the amendment being read if there is one.
    fn found(
        decisions: &mut Vec<Decision>,
        amendment: &mut Option<Amendment>,
        id: &str,
        title: &str,
        line: usize,
    ) {
        decisions.push(Decision {
            id: id.to_string(),
            title: title.to_string(),
            line,
        });
        if let Some(amendment) = amendment {
            amendment.found += 1;
        }
    }

    for (index, raw) in text.lines().enumerate() {
        let line_number = index + 1;
        let line = raw.trim_end();
        if line.starts_with("```") {
            fenced = !fenced;
            in_table = false;
            continue;
        }
        if fenced {
            continue;
        }
        if !line.starts_with('|') {
            in_table = false;
            columns = None;
        }
        if line.starts_with('#') {
            let level = line.bytes().take_while(|&b| b == b'#').count();
            let heading = line[level..].trim();
            if amendment.as_ref().is_some_and(|a| level <= a.level) {
                close(&mut amendment, &mut decisions);
            }
            if level == 1
                && let Some(rest) = heading.strip_prefix("ADR-")
            {
                let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
                number = rest[..digits].parse().ok();
                title = rest[digits..].trim_start_matches(':').trim().to_string();
            }
            if level == 2 {
                second = heading.to_string();
                if let Some(rest) = heading.strip_prefix("Decision ")
                    && let Some((digits, title)) = rest.split_once(": ")
                    && !digits.is_empty()
                    && digits.bytes().all(|b| b.is_ascii_digit())
                {
                    decisions.push(Decision {
                        id: format!("D{digits}"),
                        title: title.trim().to_string(),
                        line: line_number,
                    });
                }
            }
            if (level == 2 || level == 3)
                && let Some(rest) = heading.strip_prefix("Amendment ")
            {
                let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
                if digits > 0 {
                    close(&mut amendment, &mut decisions);
                    amendment = Some(Amendment {
                        number: rest[..digits].to_string(),
                        level,
                        title: heading
                            .split_once(": ")
                            .map_or(heading, |(_, title)| title)
                            .trim()
                            .to_string(),
                        line: line_number,
                        found: 0,
                    });
                }
            }
            if level == 3
                && let Some((id, title)) = id_and_title(heading)
            {
                found(&mut decisions, &mut amendment, id, title, line_number);
            }
            continue;
        }
        if line.starts_with('|') {
            let Some(cells) = cells(line) else {
                continue;
            };
            if !in_table {
                in_table = true;
                // The identifiers are under `#`. A table that lists decisions under "Decision",
                // as a record of what was built does, decides nothing.
                let is_decision = |c: &String| c == "Decision" || c.starts_with("Decision ");
                columns = cells
                    .iter()
                    .position(is_decision)
                    .filter(|_| cells[0] == "#")
                    .map(|decision| Columns {
                        decision,
                        question: cells.iter().position(|c| c == "Question"),
                    });
                continue;
            }
            if cells.len() == 2 && cells[0] == "Status" && cells[1].starts_with("**Accepted**") {
                accepted = true;
            }
            if let Some(columns) = &columns
                && cells.first().is_some_and(|id| is_decision_id(id))
            {
                let quoted = match columns.question.and_then(|q| cells.get(q)) {
                    Some(question) => Some(question.clone()),
                    None => cells.get(columns.decision).map(|cell| title_of(cell)),
                };
                if let Some(title) = quoted {
                    found(
                        &mut decisions,
                        &mut amendment,
                        &cells[0],
                        &title,
                        line_number,
                    );
                }
            }
            continue;
        }
        if (second.starts_with("Decisions") || second.starts_with("Proposed decisions"))
            && let Some((bold, _)) = line.strip_prefix("**").and_then(|r| r.split_once("**"))
            && let Some((id, title)) = id_and_title(bold)
        {
            found(
                &mut decisions,
                &mut amendment,
                id,
                title.trim_end_matches('.'),
                line_number,
            );
        }
        if second.starts_with("Approved defaults") && numbered(line) {
            let digits = line.bytes().take_while(u8::is_ascii_digit).count();
            decisions.push(Decision {
                id: format!("def{}", &line[..digits]),
                title: line[digits + 2..].trim().to_string(),
                line: line_number,
            });
        }
    }
    close(&mut amendment, &mut decisions);
    decisions.sort_by_key(|d| d.line);
    Some(Adr {
        number: number?,
        title,
        accepted,
        decisions,
    })
}

// ---------------------------------------------------------------------------------------------
// The decision sheet and the README
// ---------------------------------------------------------------------------------------------

/// The items of the decision sheet that are answered: a table row whose first cell is a number
/// and then `, answered`, `, your text` or `, delegated to Claude`, with or without a question's
/// number in brackets between.
fn answered(sheet: &str) -> BTreeSet<u32> {
    let mut answered = BTreeSet::new();
    for table in tables(sheet) {
        for (_, cells) in &table.rows {
            let Some(first) = cells.first() else {
                continue;
            };
            let digits = first.bytes().take_while(u8::is_ascii_digit).count();
            let rest = &first[digits..];
            let is_answer = ["answered", "your text", "delegated to Claude"]
                .iter()
                .any(|word| rest.contains(word));
            if digits > 0
                && (rest.starts_with(',') || rest.starts_with(" ("))
                && is_answer
                && let Ok(item) = first[..digits].parse()
            {
                answered.insert(item);
            }
        }
    }
    answered
}

/// One approved scope change (item 196).
#[derive(Debug)]
struct Change {
    line: usize,
    items: Vec<u32>,
    /// The rows it touched: IDs, or `prefix-*` for every row whose ID starts with the prefix.
    rows: Vec<String>,
}

/// Answered decision items that no row names.
#[derive(Debug)]
struct Unrowed {
    line: usize,
    items: Vec<u32>,
    /// Whether rows are owed for them, once a text that is not in this tree reaches it. If not,
    /// they ask for nothing.
    owed: bool,
}

/// What the gate reads from the matrix's README.
#[derive(Debug, Default)]
struct Readme {
    revision: Option<String>,
    changes: Vec<Change>,
    unrowed: Vec<Unrowed>,
    /// For each PART of the directive, the digest the README records of what no row quotes.
    unquoted: Option<BTreeMap<u8, String>>,
    /// The IDs the README lists as `done`, and as `info`.
    done: Option<BTreeSet<String>>,
    information: Option<BTreeSet<String>>,
    problems: Vec<String>,
}

fn readme(text: &str) -> Readme {
    let mut out = Readme::default();
    let (mut has_changes, mut has_unrowed) = (false, false);
    for table in tables(text) {
        if is_header(&table.header, &CHANGES) {
            has_changes = true;
            for (line, cells) in &table.rows {
                match (cells.len() == CHANGES.len()).then(|| items(&cells[0])).flatten() {
                    Some(items) if !items.is_empty() => out.changes.push(Change {
                        line: *line,
                        items,
                        rows: cells[3]
                            .split(',')
                            .map(|row| row.trim().to_string())
                            .filter(|row| !row.is_empty() && row != "-")
                            .collect(),
                    }),
                    _ => out.problems.push(format!(
                        "README.md:{line}: a scope change is four cells, the first its decision items"
                    )),
                }
            }
        } else if is_header(&table.header, &UNROWED) {
            has_unrowed = true;
            for (line, cells) in &table.rows {
                let items = (cells.len() == UNROWED.len())
                    .then(|| items(&cells[0]))
                    .flatten();
                let owed = match cells.get(1).map(String::as_str) {
                    Some("none") => Some(false),
                    Some("owed") => Some(true),
                    _ => None,
                };
                match (items, owed) {
                    (Some(items), Some(owed)) if !items.is_empty() => {
                        out.unrowed.push(Unrowed {
                            line: *line,
                            items,
                            owed,
                        });
                    }
                    _ => out.problems.push(format!(
                        "README.md:{line}: an entry of the items without rows is three cells: its decision items, `none` or `owed`, and why"
                    )),
                }
            }
        } else if is_header(&table.header, &UNQUOTED) {
            let recorded = out.unquoted.get_or_insert_with(BTreeMap::new);
            for (line, cells) in &table.rows {
                let part = cells.first().and_then(|part| part.parse::<u8>().ok());
                match (part, cells.get(1)) {
                    (Some(part), Some(digest)) if cells.len() == 2 => {
                        if recorded.insert(part, digest.clone()).is_some() {
                            out.problems
                                .push(format!("README.md:{line}: PART {part:02} is recorded twice"));
                        }
                    }
                    _ => out.problems.push(format!(
                        "README.md:{line}: a row of what the rows do not quote is a PART's number and a digest"
                    )),
                }
            }
        } else if is_header(&table.header, &LISTS) {
            for (line, cells) in &table.rows {
                let list = match cells.first().map(String::as_str) {
                    Some("done") if cells.len() == 2 => out.done.get_or_insert_with(BTreeSet::new),
                    Some("info") if cells.len() == 2 => {
                        out.information.get_or_insert_with(BTreeSet::new)
                    }
                    _ => {
                        out.problems.push(format!(
                            "README.md:{line}: a list of rows is `done` or `info`, and then the rows' IDs"
                        ));
                        continue;
                    }
                };
                for id in entries(&cells[1]) {
                    if !list.insert(id.to_string()) {
                        out.problems
                            .push(format!("README.md:{line}: the list names {id} twice"));
                    }
                }
            }
        } else {
            for (_, cells) in &table.rows {
                if cells.len() == 2 && cells[0] == REVISION_KEY {
                    out.revision = Some(cells[1].clone());
                }
            }
        }
    }
    if out.unquoted.is_none() {
        out.problems.push(
            "README.md has no table of what the rows do not quote (`| Part | Headings and code blocks |`)"
                .into(),
        );
    }
    if out.done.is_none() || out.information.is_none() {
        out.problems.push(
            "README.md has no lists of the rows that are `done` and that are `info` (`| List | Rows |`)"
                .into(),
        );
    }
    if !has_changes {
        out.problems.push(
            "README.md has no table of scope changes (`| Item | Date | What changed | Rows |`)"
                .into(),
        );
    }
    if !has_unrowed {
        out.problems.push(
            "README.md has no table of decision items without rows (`| Item | Rows | Why |`)"
                .into(),
        );
    }
    out
}

// ---------------------------------------------------------------------------------------------
// The matrix's rows
// ---------------------------------------------------------------------------------------------

/// Where evidence ran, and what a row's scope asks for, weakest first (CLAUDE.md §1, "Qualify
/// the declared environment").
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Env {
    /// Tests and tools on the development host.
    Host,
    /// QEMU's software processor.
    Emulation,
    /// A guest on a real processor through hardware-assisted virtualization, with QEMU's devices.
    Virtual,
    /// A physical machine.
    Hardware,
}

impl Env {
    fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "host" => Self::Host,
            "emu" => Self::Emulation,
            "virt" => Self::Virtual,
            "hw" => Self::Hardware,
            _ => return None,
        })
    }

    fn named(self) -> &'static str {
        match self {
            Self::Host => "host",
            Self::Emulation => "emu",
            Self::Virtual => "virt",
            Self::Hardware => "hw",
        }
    }
}

/// What a row asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    /// Something to build or deliver.
    Build,
    /// A standing rule on how the work is done.
    Rule,
    /// Nothing: the statement informs. Its row is not counted.
    Info,
}

/// How far a row has come.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum State {
    NotStarted,
    /// Something is in the tree, and no evidence for it is.
    NotVerified,
    /// Part of it is implemented, with evidence.
    Partial,
    /// Implemented, with evidence, short of the row's scope or of its acceptance criteria.
    Built,
    /// Its acceptance criteria pass within its declared scope.
    Done,
    /// Taken out of scope by a decision item. Not counted.
    Removed,
}

impl State {
    /// The state as a row of the matrix writes it.
    pub(crate) fn named(self) -> &'static str {
        match self {
            Self::NotStarted => "not-started",
            Self::NotVerified => "not-verified",
            Self::Partial => "partial",
            Self::Built => "built",
            Self::Done => "done",
            Self::Removed => "removed",
        }
    }
}

/// Who reviewed a row's implementation.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Review {
    Nobody,
    /// An AI reviewer.
    Agent,
    /// A person, by name.
    Person(String),
}

/// One reference to evidence that the gate looks for in the tree (item 194).
#[derive(Clone, Debug, PartialEq, Eq)]
enum Evidence {
    /// `file:<path>`.
    File(String),
    /// `test:<file>::<name>`.
    Test { file: String, name: String },
    /// `proof:<name>`: a boot proof.
    Proof(String),
    /// `xtask:<command>`.
    Command(String),
}

/// One row of the matrix.
#[derive(Debug)]
pub(crate) struct Row {
    /// The matrix file it is in, without the directory.
    pub(crate) file: String,
    line: usize,
    pub(crate) id: String,
    requirement: String,
    pub(crate) kind: Kind,
    /// The environment the row's scope asks its evidence to have run in.
    scope: Option<Env>,
    /// The number of the role that owns it.
    owner: u8,
    /// `None` only for a row that asks for nothing.
    pub(crate) state: Option<State>,
    implementation: Vec<String>,
    acceptance: bool,
    /// The acceptance criteria as written.
    criteria: String,
    evidence: Vec<Evidence>,
    /// Where its evidence ran.
    pub(crate) env: Option<Env>,
    review: Review,
    limitations: bool,
    decided: Vec<u32>,
}

/// A cell's entries: `-` for none, or entries separated by `, `.
fn entries(cell: &str) -> Vec<&str> {
    if cell == "-" {
        return Vec::new();
    }
    cell.split(", ")
        .map(str::trim)
        .filter(|e| !e.is_empty())
        .collect()
}

/// Whether `path` is a path inside the repository, written from its root.
fn is_repository_path(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with('/')
        && !path.contains('\\')
        && !path.contains(char::is_whitespace)
        && path.split('/').all(|part| part != ".." && part != ".")
}

/// Whether `id` is a row's ID: `H-01`, `P9-01`, `S33-07`, `F-12`, `A05-C4`.
fn is_row_id(id: &str) -> bool {
    let Some((prefix, rest)) = id.split_once('-') else {
        return false;
    };
    let digits = |text: &str| !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit());
    match prefix.as_bytes().first() {
        Some(b'H' | b'F') => prefix.len() == 1 && digits(rest),
        Some(b'P' | b'S') => digits(&prefix[1..]) && digits(rest),
        Some(b'A') => {
            digits(&prefix[1..])
                && !rest.is_empty()
                && rest.bytes().all(|b| b.is_ascii_alphanumeric())
        }
        _ => false,
    }
}

/// Reads one row, or says what is wrong with it.
fn row(file: &str, line: usize, cells: &[String]) -> std::result::Result<Row, String> {
    if cells.len() != COLUMNS.len() {
        return Err(format!(
            "a row has {} cells, and the matrix has {} columns",
            cells.len(),
            COLUMNS.len()
        ));
    }
    let [
        id,
        requirement,
        kind,
        scope,
        owner,
        state,
        implementation,
        acceptance,
        evidence,
        env,
        review,
        limitations,
        decided,
    ] = cells
    else {
        return Err("a row is thirteen cells".into());
    };
    if !is_row_id(id) {
        return Err(format!("`{id}` is not a row's ID"));
    }
    let wrong = |what: &str, cell: &str, allowed: &str| {
        format!("row {id}: {what} `{cell}` is not one of {allowed}")
    };
    if requirement.is_empty() || requirement == "-" {
        return Err(format!("row {id} quotes nothing"));
    }
    let kind = match kind.as_str() {
        "build" => Kind::Build,
        "rule" => Kind::Rule,
        "info" => Kind::Info,
        other => return Err(wrong("kind", other, "build, rule, info")),
    };
    let scope = match (scope.as_str(), kind) {
        ("-", Kind::Info) => None,
        (scope, _) => {
            let parsed = scope.split_once('/').and_then(|(edition, env)| {
                matches!(edition, "both" | "desktop" | "server" | "eng")
                    .then(|| Env::parse(env))
                    .flatten()
            });
            match parsed {
                Some(env) => Some(env),
                None => {
                    return Err(wrong(
                        "scope",
                        scope,
                        "both, desktop, server or eng, then `/`, then host, emu, virt or hw",
                    ));
                }
            }
        }
    };
    if owner == "-" || owner.is_empty() {
        return Err(format!("row {id} has no owner"));
    }
    let Ok(owner) = owner.parse::<u8>() else {
        return Err(format!("row {id}: owner `{owner}` is not a role's number"));
    };
    let state = match (state.as_str(), kind) {
        ("-", Kind::Info) => None,
        (_, Kind::Info) => return Err(format!("row {id} asks for nothing, so its state is `-`")),
        ("not-started", _) => Some(State::NotStarted),
        ("not-verified", _) => Some(State::NotVerified),
        ("partial", _) => Some(State::Partial),
        ("built", _) => Some(State::Built),
        ("done", _) => Some(State::Done),
        ("removed", _) => Some(State::Removed),
        (other, _) => {
            return Err(wrong(
                "state",
                other,
                "not-started, not-verified, partial, built, done, removed",
            ));
        }
    };
    let implementation: Vec<String> = entries(implementation)
        .into_iter()
        .map(str::to_string)
        .collect();
    if let Some(path) = implementation.iter().find(|path| !is_repository_path(path)) {
        return Err(format!(
            "row {id}: implementation `{path}` is not a path in the repository"
        ));
    }
    let mut found = Vec::new();
    for entry in entries(evidence) {
        let reference = match entry.split_once(':') {
            Some(("file", path)) if is_repository_path(path) => Evidence::File(path.to_string()),
            Some(("test", test)) => match test.split_once("::") {
                Some((file, name)) if is_repository_path(file) && !name.is_empty() => {
                    Evidence::Test {
                        file: file.to_string(),
                        name: name.to_string(),
                    }
                }
                _ => {
                    return Err(format!(
                        "row {id}: evidence `{entry}` is not `test:<file>::<name>`"
                    ));
                }
            },
            Some(("proof", name)) if !name.is_empty() => Evidence::Proof(name.to_string()),
            Some(("xtask", command)) if !command.is_empty() => {
                Evidence::Command(command.to_string())
            }
            _ => {
                return Err(format!(
                    "row {id}: evidence `{entry}` is not `file:<path>`, `test:<file>::<name>`, `proof:<name>` or `xtask:<command>`"
                ));
            }
        };
        found.push(reference);
    }
    let env = match env.as_str() {
        "-" => None,
        env => Some(Env::parse(env).ok_or_else(|| wrong("env", env, "host, emu, virt, hw"))?),
    };
    let review = match review.as_str() {
        "none" => Review::Nobody,
        "agent" => Review::Agent,
        other => match other.strip_prefix("person:").map(str::trim) {
            Some(name) if !name.is_empty() => Review::Person(name.to_string()),
            _ => return Err(wrong("review", other, "none, agent, person:<name>")),
        },
    };
    let Some(decided) = items(decided) else {
        return Err(format!(
            "row {id}: `{decided}` is not `-` or decision items such as `71–76, 88`"
        ));
    };
    Ok(Row {
        file: file.to_string(),
        line,
        id: id.clone(),
        requirement: requirement.clone(),
        kind,
        scope,
        owner,
        state,
        implementation,
        acceptance: acceptance != "-" && !acceptance.is_empty(),
        criteria: acceptance.clone(),
        evidence: found,
        env,
        review,
        limitations: limitations != "-" && !limitations.is_empty(),
        decided,
    })
}

/// Which source a file of the matrix holds the rows of.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Home {
    /// `directive-NN-<name>.md`: a PART of the directive, 00 for its header.
    Part(u8),
    /// `adr-NNNN.md`.
    Adr(u16),
}

fn home(file: &str) -> Option<Home> {
    let name = file.strip_suffix(".md")?;
    if let Some(rest) = name.strip_prefix("directive-") {
        let (part, _) = rest.split_once('-')?;
        return (part.len() == 2)
            .then(|| part.parse().ok())
            .flatten()
            .map(Home::Part);
    }
    let number = name.strip_prefix("adr-")?;
    (number.len() == 4 && number.bytes().all(|b| b.is_ascii_digit()))
        .then(|| number.parse().ok())
        .flatten()
        .map(Home::Adr)
}

// ---------------------------------------------------------------------------------------------
// The rules
// ---------------------------------------------------------------------------------------------

/// What the rules ask of the repository's tree.
trait Tree {
    /// Whether `path`, from the repository's root, is a file or a directory.
    fn exists(&self, path: &str) -> bool;
    /// The text of the file at `path`.
    fn text(&self, path: &str) -> Option<String>;
}

/// The checkout on disk.
#[derive(Debug)]
struct Disk(PathBuf);

impl Tree for Disk {
    fn exists(&self, path: &str) -> bool {
        self.0.join(path).exists()
    }

    fn text(&self, path: &str) -> Option<String> {
        fs::read_to_string(self.0.join(path)).ok()
    }
}

/// The texts the gate judges.
#[derive(Debug)]
struct Sources<'a> {
    directive: &'a str,
    /// Each ADR's file name and text.
    adrs: &'a [(String, String)],
    sheet: &'a str,
    readme: &'a str,
    /// Each file of the matrix but its README: its name and text.
    files: &'a [(String, String)],
}

/// The coverage figure and what stands beside it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Figure {
    revision: String,
    rows: usize,
    statements: usize,
    decisions: usize,
    adrs: usize,
    /// Rows that ask for nothing, and rows taken out of scope: neither is counted.
    information: usize,
    removed: usize,
    /// The rows coverage is measured over.
    counted: usize,
    /// How many of those are standing rules, and how many of the rules are done.
    rules: usize,
    rules_done: usize,
    done: usize,
    built: usize,
    partial: usize,
    not_verified: usize,
    not_started: usize,
    /// Rows with evidence, by where it ran, weakest first.
    ran: [usize; 4],
    /// Rows past `not-started`, by who reviewed them.
    by_person: usize,
    by_agent: usize,
    by_nobody: usize,
    items_answered: usize,
    items_named: usize,
    items_nothing: usize,
    items_owed: usize,
    /// How many different acceptance criteria the rows past `not-started` have between them,
    /// and how many rows the commonest one serves.
    criteria_texts: usize,
    criteria_commonest: usize,
}

/// What a hardware record does not say: the keys of [`RECORD_KEYS`] for which its tables have
/// no row with something beside the key.
fn unsaid(record: &str) -> Vec<&'static str> {
    let tables = tables(record);
    RECORD_KEYS
        .into_iter()
        .filter(|key| {
            !tables
                .iter()
                .flat_map(|table| &table.rows)
                .any(|(_, cells)| {
                    cells.len() == 2 && cells[0] == *key && !cells[1].is_empty() && cells[1] != "-"
                })
        })
        .collect()
}

/// Adds problems of one kind, listing the first [`LISTED`] and counting the rest.
fn listed(problems: &mut Vec<String>, mut found: Vec<String>) {
    let more = found.len().saturating_sub(LISTED);
    found.truncate(LISTED);
    problems.append(&mut found);
    if more > 0 {
        problems.push(format!("… and {more} more of the same kind"));
    }
}

/// The rows of the matrix's files, `(name, text)` without the README, each file's rows in their
/// order. What cannot be read is added to `problems`, and its rows are left out. Every reader of
/// the matrix reads it here: this gate, the roadmap's and the progress page.
pub(crate) fn matrix_rows(files: &[(String, String)], problems: &mut Vec<String>) -> Vec<Row> {
    let mut rows: Vec<Row> = Vec::new();
    for (file, text) in files {
        let at = format!("{MATRIX}/{file}");
        if home(file).is_none() {
            problems.push(format!(
                "{at} is not a file of the matrix: `directive-NN-<name>.md`, `adr-NNNN.md` or `README.md`"
            ));
            continue;
        }
        let found = tables(text);
        let [table] = found.as_slice() else {
            problems.push(format!(
                "{at} holds {} tables, and a file of the matrix holds one",
                found.len()
            ));
            continue;
        };
        if !is_header(&table.header, &COLUMNS) {
            problems.push(format!(
                "{at}: the table's header is not `| {} |`",
                COLUMNS.join(" | ")
            ));
            continue;
        }
        for (line, cells) in &table.rows {
            match row(file, *line, cells) {
                Ok(row) => rows.push(row),
                Err(problem) => problems.push(format!("{at}:{line}: {problem}")),
            }
        }
    }
    rows
}

/// Each numbered section of the directive and its title, as its heading `## N. Title` gives it.
pub(crate) fn section_titles(directive_text: &str) -> BTreeMap<u8, String> {
    directive(directive_text).titles
}

/// Each ADR's number, its title, and whether it is accepted, from `(name, text)` pairs. An ADR
/// with no `# ADR-NNNN` title is left out: the requirements gate refuses it.
pub(crate) fn adr_titles(adrs: &[(String, String)]) -> BTreeMap<u16, (String, bool)> {
    adrs.iter()
        .filter_map(|(_, text)| adr(text))
        .map(|adr| (adr.number, (adr.title, adr.accepted)))
        .collect()
}

/// Everything wrong with the matrix, and its figure.
fn problems(sources: &Sources<'_>, tree: &dyn Tree) -> (Vec<String>, Figure) {
    let mut problems = Vec::new();
    let mut figure = Figure::default();

    // The sources.
    let directive = directive(sources.directive);
    problems.extend(
        directive
            .problems
            .iter()
            .map(|p| format!("{DIRECTIVE}: {p}")),
    );
    if directive.statements.is_empty() {
        problems.push(format!("{DIRECTIVE}: no statement was read"));
    }
    if directive.roles.is_empty() {
        problems.push(format!(
            "{DIRECTIVE}: no role was read from §1, so no row's owner can be checked"
        ));
    }
    let mut decisions: BTreeMap<(u16, &str), &Decision> = BTreeMap::new();
    let mut adrs = Vec::new();
    for (name, text) in sources.adrs {
        match adr(text) {
            Some(adr) => adrs.push(adr),
            None => problems.push(format!("{ADRS}/{name}: no `# ADR-NNNN` title was read")),
        }
    }
    let mut unaccepted = BTreeSet::new();
    for adr in &adrs {
        if !adr.accepted {
            unaccepted.insert(adr.number);
            continue;
        }
        figure.adrs += 1;
        for decision in &adr.decisions {
            if decisions
                .insert((adr.number, decision.id.as_str()), decision)
                .is_some()
            {
                problems.push(format!(
                    "ADR-{:04} has two decisions {}: a row cannot tell them apart",
                    adr.number, decision.id
                ));
            }
        }
    }
    let answered = answered(sources.sheet);
    if answered.is_empty() {
        problems.push(format!("{SHEET}: no answered item was read"));
    }
    let readme = readme(sources.readme);
    problems.extend(readme.problems.iter().map(|p| format!("{MATRIX}/{p}")));
    figure.revision = directive.revision.clone().unwrap_or_default();
    match (&directive.revision, &readme.revision) {
        (None, _) => problems.push(format!("{DIRECTIVE}: no `**Revision:**` line was read")),
        (Some(_), None) => problems.push(format!(
            "{MATRIX}/README.md: no `| {REVISION_KEY} | … |` row says which revision the matrix was checked against"
        )),
        (Some(directive), Some(matrix)) if directive != matrix => problems.push(format!(
            "{MATRIX}/README.md says the matrix was checked against revision {matrix}, and {DIRECTIVE} is revision {directive}: go through what the revision changed, then change the README's row"
        )),
        _ => {}
    }

    // The rows.
    let rows = matrix_rows(sources.files, &mut problems);
    if rows.is_empty() {
        problems.push("no row was read: nothing was checked".to_string());
        return (problems, figure);
    }
    figure.rows = rows.len();
    let mut ids: BTreeMap<&str, &Row> = BTreeMap::new();
    for row in &rows {
        if let Some(first) = ids.insert(&row.id, row) {
            problems.push(format!(
                "{MATRIX}/{}:{}: the ID {} is also at {}:{}",
                row.file, row.line, row.id, first.file, first.line
            ));
        }
    }

    // Each row by itself.
    let mut texts: BTreeMap<&str, Option<String>> = BTreeMap::new();
    let mut named: BTreeSet<u32> = BTreeSet::new();
    for row in &rows {
        let at = format!("{MATRIX}/{}:{}: row {}", row.file, row.line, row.id);
        if !directive.roles.is_empty() && !directive.roles.contains(&row.owner) {
            problems.push(format!(
                "{at}: owner {} is not one of §1's {} roles",
                row.owner,
                directive.roles.len()
            ));
        }
        for item in &row.decided {
            named.insert(*item);
            if !answered.contains(item) {
                problems.push(format!(
                    "{at} names decision item {item}, which {SHEET} does not hold as answered"
                ));
            }
        }
        for path in &row.implementation {
            if !tree.exists(path) {
                problems.push(format!(
                    "{at}: its implementation `{path}` is not in the tree"
                ));
            }
        }
        for evidence in &row.evidence {
            let mut contains = |path: &'static str, needle: &str| {
                texts
                    .entry(path)
                    .or_insert_with(|| tree.text(path))
                    .as_deref()
                    .is_some_and(|text| text.contains(needle))
            };
            let missing = match evidence {
                Evidence::File(path) => (!tree.exists(path)).then(|| format!("the file `{path}`")),
                Evidence::Test { file, name } => (!tree
                    .text(file)
                    .is_some_and(|text| text.contains(&format!("fn {name}("))))
                .then(|| format!("a test `{name}` in `{file}`")),
                Evidence::Proof(name) => (!contains(PROOFS, &format!("\"{name}\" =>")))
                    .then(|| format!("a boot proof named \"{name}\" in {PROOFS}")),
                Evidence::Command(command) => (!contains(COMMANDS, &format!("[\"{command}\"")))
                    .then(|| format!("a `cargo xtask {command}` command in {COMMANDS}")),
            };
            if let Some(missing) = missing {
                problems.push(format!(
                    "{at} names evidence that is not in the tree: {missing}"
                ));
            }
        }
        let Some(state) = row.state else {
            figure.information += 1;
            continue;
        };
        let has_evidence = !row.evidence.is_empty();
        let has_implementation = !row.implementation.is_empty();
        let mut need = |ok: bool, what: &str| {
            if !ok {
                problems.push(format!("{at}: {what}"));
            }
        };
        match state {
            State::Removed => {
                figure.removed += 1;
                need(
                    !row.decided.is_empty(),
                    "a removed row names the decision item that took it out of scope (item 196)",
                );
                continue;
            }
            State::NotStarted => {
                figure.not_started += 1;
                need(
                    !has_implementation
                        && !has_evidence
                        && row.env.is_none()
                        && row.review == Review::Nobody,
                    "a row that is not started has no implementation, evidence, environment or review: if something is in the tree, the state is `not-verified` or more",
                );
            }
            State::NotVerified => {
                figure.not_verified += 1;
                need(
                    has_implementation,
                    "a row that is not verified names what is in the tree for it",
                );
                need(
                    !has_evidence && row.env.is_none(),
                    "a row with evidence is `partial`, `built` or `done`, not `not-verified`",
                );
            }
            State::Partial | State::Built | State::Done => {
                match state {
                    State::Partial => figure.partial += 1,
                    State::Built => figure.built += 1,
                    _ => figure.done += 1,
                }
                need(has_implementation, "it names no implementation");
                need(has_evidence, "it names no evidence (item 194)");
                need(
                    row.env.is_some(),
                    "it does not say where its evidence ran (item 193)",
                );
                if state == State::Done {
                    need(
                        row.env >= row.scope,
                        &format!(
                            "it is `done` on evidence that ran on `{}`, and its scope asks for `{}` (item 193): the state is `built`",
                            row.env.map_or("-", Env::named),
                            row.scope.map_or("-", Env::named)
                        ),
                    );
                } else {
                    need(
                        row.limitations,
                        "a `partial` or `built` row says in its limitations what keeps it from `done`",
                    );
                }
            }
        }
        if row.kind == Kind::Rule {
            figure.rules += 1;
            figure.rules_done += usize::from(state == State::Done);
        }
        if state != State::NotStarted {
            need(
                row.acceptance,
                "it has left `not-started` without acceptance criteria (item 195)",
            );
            match row.review {
                Review::Person(_) => figure.by_person += 1,
                Review::Agent => figure.by_agent += 1,
                Review::Nobody => figure.by_nobody += 1,
            }
        }
        if let Some(env) = row.env {
            figure.ran[env as usize] += 1;
            if env == Env::Hardware {
                let records: Vec<&String> = row
                    .evidence
                    .iter()
                    .filter_map(|e| match e {
                        Evidence::File(path) if path.starts_with(HARDWARE_RECORDS) => Some(path),
                        _ => None,
                    })
                    .collect();
                need(
                    !records.is_empty(),
                    &format!(
                        "its evidence ran on hardware, and it names no record under {HARDWARE_RECORDS} (`file:{HARDWARE_RECORDS}…`)"
                    ),
                );
                for path in records {
                    let missing = tree.text(path).map_or_else(Vec::new, |text| unsaid(&text));
                    need(
                        missing.is_empty(),
                        &format!(
                            "its hardware record `{path}` does not say: {}. A record is a table with a row for each of {}",
                            missing.join(", "),
                            RECORD_KEYS.join(", ")
                        ),
                    );
                }
            }
        }
    }
    figure.counted = figure.rows - figure.information - figure.removed;
    let mut criteria: BTreeMap<&str, usize> = BTreeMap::new();
    for row in &rows {
        if row.acceptance && !matches!(row.state, None | Some(State::NotStarted | State::Removed)) {
            *criteria.entry(row.criteria.as_str()).or_default() += 1;
        }
    }
    figure.criteria_texts = criteria.len();
    figure.criteria_commonest = criteria.values().copied().max().unwrap_or(0);

    // The rows against their sources: every statement and decision has one row, in the file of
    // its source and in its order, and no row quotes what its source does not say.
    figure.statements = directive.statements.len();
    figure.decisions = decisions.len();
    let mut parts: BTreeMap<String, u8> = BTreeMap::new();
    let mut waiting: BTreeMap<(String, &str), Vec<usize>> = BTreeMap::new();
    for (index, statement) in directive.statements.iter().enumerate().rev() {
        parts.insert(statement.section.prefix(), statement.part);
        waiting
            .entry((statement.section.prefix(), statement.text.as_str()))
            .or_default()
            .push(index);
    }
    let mut claimed: BTreeSet<(u16, &str)> = BTreeSet::new();
    let mut stale = Vec::new();
    let mut misplaced = Vec::new();
    // The place in its source of the row before, in the same file.
    let mut before: Option<(&str, usize)> = None;
    for row in &rows {
        let at = format!("{MATRIX}/{}:{}: row {}", row.file, row.line, row.id);
        let removed = row.state == Some(State::Removed);
        let (prefix, suffix) = row.id.split_once('-').unwrap_or((&row.id, ""));
        // A row in another source's file is said once, and its order there is not judged.
        let mut at_home = true;
        let place = if let Some(number) = prefix.strip_prefix('A') {
            let number: u16 = number.parse().unwrap_or(u16::MAX);
            if home(&row.file) != Some(Home::Adr(number)) {
                at_home = false;
                misplaced.push(format!("{at} belongs in adr-{number:04}.md"));
            }
            match decisions.get(&(number, suffix)) {
                Some(decision) if decision.title == row.requirement => {
                    claimed.insert((number, decision.id.as_str()));
                    Some(decision.line)
                }
                Some(decision) => {
                    stale.push(format!(
                        "{at} does not quote ADR-{number:04}'s {suffix} as the ADR has it (line {}): {}",
                        decision.line,
                        escaped(&decision.title)
                    ));
                    claimed.insert((number, decision.id.as_str()));
                    None
                }
                None if removed => None,
                None if unaccepted.contains(&number) => {
                    stale.push(format!(
                        "{at} is a row for ADR-{number:04}, which is not accepted"
                    ));
                    None
                }
                None => {
                    stale.push(format!("{at}: ADR-{number:04} has no decision {suffix}"));
                    None
                }
            }
        } else {
            if let Some(part) = parts.get(prefix)
                && home(&row.file) != Some(Home::Part(*part))
            {
                at_home = false;
                misplaced.push(format!(
                    "{at} belongs in the file of PART {part}, directive-{part:02}-<name>.md"
                ));
            }
            match waiting
                .get_mut(&(prefix.to_string(), row.requirement.as_str()))
                .and_then(Vec::pop)
            {
                Some(index) => Some(directive.statements[index].line),
                None if removed => None,
                None => {
                    stale.push(format!(
                        "{at} quotes a statement that {DIRECTIVE} does not make there, or that another row already quotes"
                    ));
                    None
                }
            }
        };
        if let Some(place) = place
            && at_home
        {
            if let Some((file, last)) = before
                && file == row.file
                && place < last
            {
                misplaced.push(format!(
                    "{at} is out of its source's order: it quotes line {place}, and the row before it line {last}"
                ));
            }
            before = Some((row.file.as_str(), place));
        }
    }
    listed(&mut problems, stale);
    listed(&mut problems, misplaced);
    let mut highest: BTreeMap<String, u32> = BTreeMap::new();
    for row in &rows {
        if let Some((prefix, number)) = row.id.split_once('-')
            && let Ok(number) = number.parse::<u32>()
        {
            let highest = highest.entry(prefix.to_string()).or_default();
            *highest = (*highest).max(number);
        }
    }
    let mut unrowed = Vec::new();
    let mut indexes: Vec<usize> = waiting.into_values().flatten().collect();
    indexes.sort_unstable();
    for index in indexes {
        let statement = &directive.statements[index];
        let prefix = statement.section.prefix();
        let next = highest.entry(prefix.clone()).or_default();
        *next += 1;
        unrowed.push(format!(
            "{DIRECTIVE}:{}: a statement of {} has no row. Its row starts: | {prefix}-{next:02} | {} |",
            statement.line,
            statement.section.named(),
            escaped(&statement.text)
        ));
    }
    for ((number, id), decision) in &decisions {
        if !claimed.contains(&(*number, *id)) {
            unrowed.push(format!(
                "ADR-{number:04}'s decision {id} (line {}) has no row. Its row starts: | A{number:02}-{id} | {} |",
                decision.line,
                escaped(&decision.title)
            ));
        }
    }
    listed(&mut problems, unrowed);

    // Decision items: each answered one is named by a row, or listed in the README (item 191).
    let mut nothing = BTreeSet::new();
    let mut owed = BTreeSet::new();
    for entry in &readme.unrowed {
        for item in &entry.items {
            let at = format!("{MATRIX}/README.md:{}: decision item {item}", entry.line);
            if !answered.contains(item) {
                problems.push(format!(
                    "{at} is listed without rows, and {SHEET} does not hold it as answered"
                ));
            }
            if named.contains(item) {
                problems.push(format!(
                    "{at} is listed without rows, and a row names it: take it off the list"
                ));
            }
            if !(if entry.owed { &mut owed } else { &mut nothing }).insert(*item) {
                problems.push(format!("{at} is listed twice"));
            }
        }
    }
    figure.items_answered = answered.len();
    figure.items_nothing = nothing.len();
    figure.items_owed = owed.len();
    figure.items_named = answered.intersection(&named).count();
    let unnamed: Vec<String> = answered
        .iter()
        .filter(|item| !named.contains(item) && !nothing.contains(item) && !owed.contains(item))
        .map(u32::to_string)
        .collect();
    if !unnamed.is_empty() {
        problems.push(format!(
            "answered decision items that no row names and the README does not list: {}. Name each in the last cell of the row it decided, or list it in the README's table of items without rows",
            unnamed.join(", ")
        ));
    }

    // What no row quotes: each PART's headings, code blocks and table headers are as the README
    // recorded them, so a change to one is seen and read against that PART's rows.
    if let Some(recorded) = &readme.unquoted {
        for (part, now) in &directive.unquoted {
            match recorded.get(part) {
                Some(then) if then == now => {}
                then => problems.push(format!(
                    "{DIRECTIVE}: the headings, code blocks or table headers of PART {part:02} are not as {MATRIX}/README.md records them (they give SHA-256 {now}, and the README has {}). Read the change against the rows of directive-{part:02}-<name>.md, then record the new digest",
                    then.map_or("none", String::as_str)
                )),
            }
        }
        for part in recorded
            .keys()
            .filter(|part| !directive.unquoted.contains_key(part))
        {
            problems.push(format!(
                "{MATRIX}/README.md records a digest for PART {part:02}, which {DIRECTIVE} does not have"
            ));
        }
    }

    // The rows that move the figure, `done` and `info`, are the ones the README lists, so a row
    // joins or leaves either by two edits.
    for (what, listed_ids, is) in [
        (
            "done",
            &readme.done,
            (|row: &Row| row.state == Some(State::Done)) as fn(&Row) -> bool,
        ),
        ("info", &readme.information, |row: &Row| {
            row.kind == Kind::Info
        }),
    ] {
        let Some(listed_ids) = listed_ids else {
            continue;
        };
        for row in rows
            .iter()
            .filter(|row| is(row) && !listed_ids.contains(&row.id))
        {
            problems.push(format!(
                "{MATRIX}/{}:{}: row {} is `{what}`, and the README's list of `{what}` rows does not name it: a row joins that list by an edit of its own",
                row.file, row.line, row.id
            ));
        }
        for id in listed_ids {
            if !ids.get(id.as_str()).is_some_and(|row| is(row)) {
                problems.push(format!(
                    "{MATRIX}/README.md lists {id} as `{what}`, and the matrix has no such row that is"
                ));
            }
        }
    }

    // Scope changes: their items are answered, their rows exist, and they name every removed row.
    let touches = |change: &Change, id: &str| {
        change.rows.iter().any(|row| match row.strip_suffix('*') {
            Some(prefix) => id.starts_with(prefix),
            None => row == id,
        })
    };
    for change in &readme.changes {
        let at = format!("{MATRIX}/README.md:{}: a scope change", change.line);
        for item in &change.items {
            if !answered.contains(item) {
                problems.push(format!(
                    "{at} names decision item {item}, which {SHEET} does not hold as answered"
                ));
            }
        }
        if change.rows.is_empty() {
            problems.push(format!("{at} names no row"));
        }
        for row in &change.rows {
            let known = match row.strip_suffix('*') {
                Some(prefix) => rows.iter().any(|r| r.id.starts_with(prefix)),
                None => ids.contains_key(row.as_str()),
            };
            if !known {
                problems.push(format!(
                    "{at} names `{row}`, and the matrix has no such row"
                ));
            }
        }
    }
    for row in rows.iter().filter(|row| row.state == Some(State::Removed)) {
        let recorded = readme.changes.iter().any(|change| {
            touches(change, &row.id) && change.items.iter().any(|item| row.decided.contains(item))
        });
        if !recorded && !row.decided.is_empty() {
            problems.push(format!(
                "{MATRIX}/{}:{}: row {} is removed, and no scope change in the README names it with its decision item (item 196)",
                row.file, row.line, row.id
            ));
        }
    }
    (problems, figure)
}

/// `part` of `whole`, as a percentage with one decimal.
pub(crate) fn percent(part: usize, whole: usize) -> String {
    if whole == 0 {
        return "0.0%".to_string();
    }
    let tenths = (part * 1000 + whole / 2) / whole;
    format!("{}.{}%", tenths / 10, tenths % 10)
}

/// The figure as the gate prints it.
fn report(figure: &Figure) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "xtask: requirements gate: {} rows hold the {} statements of CLAUDE.md {} and the {} decisions of {} accepted ADRs, one each; every implementation and piece of evidence they name is in the tree",
        figure.rows, figure.statements, figure.revision, figure.decisions, figure.adrs
    );
    let _ = writeln!(
        out,
        "xtask: requirements coverage: {} of {} counted rows are done ({}); built {}, partial {}, not verified {}, not started {}; {} of the counted rows are standing rules, {} of them done; not counted: {} rows that ask for nothing and {} removed from scope",
        figure.done,
        figure.counted,
        percent(figure.done, figure.counted),
        figure.built,
        figure.partial,
        figure.not_verified,
        figure.not_started,
        figure.rules,
        figure.rules_done,
        figure.information,
        figure.removed
    );
    let _ = writeln!(
        out,
        "xtask: requirements evidence ran: on the host for {} rows, under emulation for {}, in a virtual machine on a real processor for {}, on physical hardware for {}; of the {} rows past not-started, a person reviewed {}, an AI reviewer {}, nobody {}",
        figure.ran[Env::Host as usize],
        figure.ran[Env::Emulation as usize],
        figure.ran[Env::Virtual as usize],
        figure.ran[Env::Hardware as usize],
        figure.by_person + figure.by_agent + figure.by_nobody,
        figure.by_person,
        figure.by_agent,
        figure.by_nobody
    );
    let _ = writeln!(
        out,
        "xtask: requirements decisions: of {} answered decision items, rows name {}, {} are listed as asking for nothing and {} as owed rows",
        figure.items_answered, figure.items_named, figure.items_nothing, figure.items_owed
    );
    let _ = write!(
        out,
        "xtask: requirements criteria: the {} rows past not-started have {} different acceptance criteria between them, and the commonest serves {} rows: a row has a criterion of its own once its task's contract writes one",
        figure.by_person + figure.by_agent + figure.by_nobody,
        figure.criteria_texts,
        figure.criteria_commonest
    );
    out
}

/// The matrix and its sources as the checkout holds them.
#[derive(Debug)]
pub(crate) struct Read {
    pub(crate) directive: String,
    /// Each ADR's file name and text.
    pub(crate) adrs: Vec<(String, String)>,
    sheet: String,
    readme: String,
    /// Each file of the matrix but its README: its name and text, sorted by name.
    pub(crate) files: Vec<(String, String)>,
}

/// Reads the matrix and its sources from the checkout.
pub(crate) fn read() -> Result<Read> {
    let root = repo_root();
    let text =
        |path: PathBuf| fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()));
    let markdown =
        |directory: &str, keep: &dyn Fn(&str) -> bool| -> Result<Vec<(String, String)>> {
            let path = root.join(directory);
            let mut names = Vec::new();
            for entry in fs::read_dir(&path).map_err(|e| format!("{}: {e}", path.display()))? {
                let name = entry
                    .map_err(|e| format!("{}: {e}", path.display()))?
                    .file_name();
                let name = name.to_string_lossy().into_owned();
                if name.ends_with(".md") && keep(&name) {
                    names.push(name);
                }
            }
            names.sort();
            names
                .into_iter()
                .map(|name| Ok((name.clone(), text(path.join(&name))?)))
                .collect()
        };
    Ok(Read {
        directive: text(root.join(DIRECTIVE))?,
        adrs: markdown(ADRS, &|name| name.starts_with("ADR-"))?,
        sheet: text(root.join(SHEET))?,
        readme: text(root.join(MATRIX).join("README.md"))?,
        files: markdown(MATRIX, &|name| name != "README.md")?,
    })
}

/// The figure of this checkout's matrix, for the progress page's test that its counts agree:
/// counted rows, done, built, in progress (`partial` and `not-verified`), not started, and the
/// rows whose evidence ran in each environment.
#[cfg(test)]
pub(crate) fn tree_counts() -> (usize, usize, usize, usize, usize, [usize; 4]) {
    let read = read().expect("the matrix and its sources");
    let sources = Sources {
        directive: &read.directive,
        adrs: &read.adrs,
        sheet: &read.sheet,
        readme: &read.readme,
        files: &read.files,
    };
    let (_, figure) = problems(&sources, &Disk(repo_root()));
    (
        figure.counted,
        figure.done,
        figure.built,
        figure.partial + figure.not_verified,
        figure.not_started,
        figure.ran,
    )
}

/// The gate: the matrix is complete and true to the tree, and its figure is printed.
pub(crate) fn gate() -> Result {
    let read = read()?;
    let sources = Sources {
        directive: &read.directive,
        adrs: &read.adrs,
        sheet: &read.sheet,
        readme: &read.readme,
        files: &read.files,
    };
    let (problems, figure) = problems(&sources, &Disk(repo_root()));
    if !problems.is_empty() {
        return Err(format!(
            "the requirements matrix under {MATRIX}/ is not complete and true to the tree (CLAUDE.md §1; its rules are in {MATRIX}/README.md):\n  {}",
            problems.join("\n  ")
        ));
    }
    println!("{}", report(&figure));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A small directive with every shape the statement rule names.
    const DIRECTIVE_TEXT: &str = "\
# ORIGIN OS

**Product:** Origin OS  
**Revision:** 9.1, 2026-01-01. Revision 9.0 said less.  

| Extension | Purpose |
|---|---|
| `.ose` | Origin System Executable |

---

# PART I — AUTHORITY

## 1. Your mission

Exercise the following roles.

1. **First Architect.** Own the first things.
2. **Second Architect.** Own the second things.

## 2. Priorities

**P0 — Correctness**

Memory safety.

# PART II — BUILD

The formats are profiles.

## 3. Boot

Implement:

- A loader.
- A kernel entry.

Use `rust-toolchain.toml`:

```toml
channel = \"1.99.1\"
```

The valid architecture is:

Client → SDK.

Do not stop.

# FINAL MANDATORY ENGINEERING RULES

1. The kernel must boot.

**BEGIN.**
";

    const ADR_TEXT: &str = "\
# ADR-0001: A thing

| | |
|---|---|
| Status | **Accepted** by the owner |

## Decisions

### K1. The first

Its body.

## Amendment 1 (Accepted): more

| # | Decision |
|---|---|
| A1 | **Bold title,** and what follows it. |

| # | Threat | Defence |
|---|---|---|
| S1 | A threat | A defence |

## Amendment 2 (Accepted): a single change

Its text.
";

    const SHEET_TEXT: &str = "\
| # | Question | Answer |
|---|---|---|
| 1, answered | A question? | **Yes** |
| 2 (Q3), answered | Another? | **Yes** |
| 3, delegated to Claude (\"You decide\") | A process rule? | **Yes** |
| 4, open | Not yet? | |
";

    const README_TEXT: &str = "\
# The matrix

| | |
|---|---|
| Directive revision | 9.1 |

| Item | Date | What changed | Rows |
|---|---|---|---|
| 2 | 2026-01-01 | Stopping is allowed. | S3-05 |

| Item | Rows | Why |
|---|---|---|
| 3 | none | A process rule. |

| List | Rows |
|---|---|
| done | S3-03 |
| info | H-01, H-02, S1-01 |
| info | P2-01, F-02 |
";

    const HEAD: &str = "\
| ID | Requirement | Kind | Scope | Owner | State | Implementation | Acceptance | Evidence | Env | Review | Limitations | Decided by |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
";

    const FILES: [(&str, &str); 5] = [
        (
            "directive-00-header.md",
            "\
| H-01 | **Product:** Origin OS | info | - | 1 | - | - | - | - | - | none | - | - |
| H-02 | **Revision:** 9.1, 2026-01-01. Revision 9.0 said less. | info | - | 1 | - | - | - | - | - | none | - | - |
| H-03 | `.ose`: Origin System Executable | build | both/hw | 2 | not-started | - | - | - | - | none | - | - |
",
        ),
        (
            "directive-01-authority.md",
            "\
| S1-01 | Exercise the following roles. | info | - | 1 | - | - | - | - | - | none | - | - |
| S1-02 | 1. **First Architect.** Own the first things. | build | eng/host | 1 | not-started | - | - | - | - | none | - | - |
| S1-03 | 2. **Second Architect.** Own the second things. | build | eng/host | 2 | not-started | - | - | - | - | none | - | - |
| S2-01 | **P0 — Correctness** Memory safety. | rule | both/hw | 1 | not-started | - | - | - | - | none | - | - |
",
        ),
        (
            "directive-02-build.md",
            "\
| P2-01 | The formats are profiles. | info | - | 2 | - | - | - | - | - | none | - | - |
| S3-01 | Implement: A loader. | build | both/hw | 2 | built | boot/loader.rs | The loader's proof passes on a declared machine. | proof:the loader, test:boot/loader.rs::loads, xtask:boot-test | emu | agent | QEMU only. | - |
| S3-02 | Implement: A kernel entry. | build | both/hw | 2 | partial | boot | The entry's proof passes. | proof:the loader | emu | none | No entry on a second processor. | - |
| S3-03 | Use `rust-toolchain.toml`: | rule | eng/host | 1 | done | rust-toolchain.toml | The file pins the compiler. | file:rust-toolchain.toml | host | person:Ada | - | 1 |
| S3-04 | The valid architecture is: Client → SDK. | build | both/hw | 2 | not-verified | docs/x.md | A gate holds it. | - | - | none | Nothing checks it. | - |
| S3-05 | Do not stop. | rule | eng/host | 1 | removed | - | - | - | - | none | Taken out of scope. | 2 |
",
        ),
        (
            "directive-03-final.md",
            "\
| F-01 | 1. The kernel must boot. | build | both/hw | 2 | not-started | - | - | - | - | none | - | - |
| F-02 | **BEGIN.** | info | - | 1 | - | - | - | - | - | none | - | - |
",
        ),
        (
            "adr-0001.md",
            "\
| A01-K1 | The first | build | both/hw | 2 | not-started | - | - | - | - | none | - | 1 |
| A01-A1 | Bold title | build | both/hw | 2 | not-started | - | - | - | - | none | - | - |
| A01-Am2 | a single change | build | both/hw | 2 | not-started | - | - | - | - | none | - | - |
",
        ),
    ];

    /// A tree of named files.
    impl Tree for BTreeMap<String, String> {
        fn exists(&self, path: &str) -> bool {
            let inside = format!("{}/", path.trim_end_matches('/'));
            self.keys()
                .any(|file| file == path || file.starts_with(&inside))
        }

        fn text(&self, path: &str) -> Option<String> {
            self.get(path).cloned()
        }
    }

    /// A matrix, its sources and its tree.
    struct Case {
        directive: String,
        adrs: Vec<(String, String)>,
        sheet: String,
        readme: String,
        files: Vec<(String, String)>,
        tree: BTreeMap<String, String>,
        /// The directive the README's digests were recorded from: the case's own, unless a
        /// test sets an older one. `Some("")` leaves the table out.
        recorded_from: Option<String>,
    }

    /// The README's table of what the rows do not quote, as recorded from `directive`.
    fn digests(directive: &str) -> String {
        let mut table = String::from("\n| Part | Headings and code blocks |\n|---|---|\n");
        for (part, digest) in unquoted(directive) {
            table.push_str(&format!("| {part:02} | {digest} |\n"));
        }
        table
    }

    impl Case {
        /// A matrix with nothing wrong: every state, kind and kind of evidence is in it.
        fn sound() -> Self {
            let tree = [
                ("boot/loader.rs", "#[test]\nfn loads() {}\n"),
                ("rust-toolchain.toml", "channel = \"1.99.1\"\n"),
                ("docs/x.md", "A note.\n"),
                (PROOFS, "    Loader \"the loader\" => Refuse;\n"),
                (
                    COMMANDS,
                    "        [\"boot-test\"] => in_turn(\"boot-test\", boot_test),\n",
                ),
            ];
            Self {
                directive: DIRECTIVE_TEXT.to_string(),
                adrs: vec![("ADR-0001-a-thing.md".to_string(), ADR_TEXT.to_string())],
                sheet: SHEET_TEXT.to_string(),
                readme: README_TEXT.to_string(),
                files: FILES
                    .iter()
                    .map(|(name, rows)| ((*name).to_string(), format!("# Rows\n\n{HEAD}{rows}")))
                    .collect(),
                tree: tree
                    .iter()
                    .map(|(path, text)| ((*path).to_string(), (*text).to_string()))
                    .collect(),
                recorded_from: None,
            }
        }

        fn judged(&self) -> (Vec<String>, Figure) {
            let recorded = match self.recorded_from.as_deref() {
                Some("") => String::new(),
                Some(older) => digests(older),
                None => digests(&self.directive),
            };
            let readme = format!("{}{recorded}", self.readme);
            let sources = Sources {
                directive: &self.directive,
                adrs: &self.adrs,
                sheet: &self.sheet,
                readme: &readme,
                files: &self.files,
            };
            problems(&sources, &self.tree)
        }

        /// Replaces `from`, which must be there once, in the file of the matrix named `file`.
        fn edit(mut self, file: &str, from: &str, to: &str) -> Self {
            let text = &mut self
                .files
                .iter_mut()
                .find(|(name, _)| name == file)
                .unwrap_or_else(|| panic!("no file {file}"))
                .1;
            assert_eq!(
                text.matches(from).count(),
                1,
                "`{from}` is not once in {file}"
            );
            *text = text.replace(from, to);
            self
        }

        /// Asserts that the matrix is refused, with a problem that contains `needle`.
        fn refused(&self, needle: &str) {
            let (found, _) = self.judged();
            assert!(
                found.iter().any(|problem| problem.contains(needle)),
                "no problem contains `{needle}`: {found:#?}"
            );
        }

        /// Asserts that the matrix is refused for one thing only, which contains `needle`.
        fn refused_once(&self, needle: &str) {
            let (found, _) = self.judged();
            assert!(
                found.len() == 1 && found[0].contains(needle),
                "not one problem that contains `{needle}`: {found:#?}"
            );
        }
    }

    fn once(text: &str, from: &str, to: &str) -> String {
        assert_eq!(text.matches(from).count(), 1, "`{from}` is not there once");
        text.replace(from, to)
    }

    #[test]
    fn a_sound_matrix_passes_and_is_counted() {
        let (found, figure) = Case::sound().judged();
        assert_eq!(found, Vec::<String>::new());
        assert_eq!(
            figure,
            Figure {
                revision: "9.1".to_string(),
                rows: 18,
                statements: 15,
                decisions: 3,
                adrs: 1,
                information: 5,
                removed: 1,
                counted: 12,
                rules: 2,
                rules_done: 1,
                done: 1,
                built: 1,
                partial: 1,
                not_verified: 1,
                not_started: 8,
                ran: [1, 2, 0, 0],
                by_person: 1,
                by_agent: 1,
                by_nobody: 2,
                items_answered: 3,
                items_named: 2,
                items_nothing: 1,
                items_owed: 0,
                criteria_texts: 4,
                criteria_commonest: 1,
            }
        );
        let report = report(&figure);
        assert!(
            report.contains("the 4 rows past not-started have 4 different acceptance criteria between them, and the commonest serves 1 rows"),
            "{report}"
        );
        assert!(
            report.contains("1 of 12 counted rows are done (8.3%)"),
            "{report}"
        );
        assert!(
            report.contains("a person reviewed 1, an AI reviewer 1, nobody 2"),
            "{report}"
        );
    }

    #[test]
    fn the_statements_of_a_directive() {
        let read = directive(DIRECTIVE_TEXT);
        assert_eq!(read.problems, Vec::<String>::new());
        assert_eq!(read.revision.as_deref(), Some("9.1"));
        assert_eq!(read.roles, BTreeSet::from([1, 2]));
        let statements: Vec<(String, u8, &str)> = read
            .statements
            .iter()
            .map(|s| (s.section.prefix(), s.part, s.text.as_str()))
            .collect();
        let expected = [
            ("H", 0, "**Product:** Origin OS"),
            (
                "H",
                0,
                "**Revision:** 9.1, 2026-01-01. Revision 9.0 said less.",
            ),
            ("H", 0, "`.ose`: Origin System Executable"),
            ("S1", 1, "Exercise the following roles."),
            ("S1", 1, "1. **First Architect.** Own the first things."),
            ("S1", 1, "2. **Second Architect.** Own the second things."),
            ("S2", 1, "**P0 — Correctness** Memory safety."),
            ("P2", 2, "The formats are profiles."),
            ("S3", 2, "Implement: A loader."),
            ("S3", 2, "Implement: A kernel entry."),
            ("S3", 2, "Use `rust-toolchain.toml`:"),
            ("S3", 2, "The valid architecture is: Client → SDK."),
            ("S3", 2, "Do not stop."),
            ("F", 3, "1. The kernel must boot."),
            ("F", 3, "**BEGIN.**"),
        ];
        let expected: Vec<(String, u8, &str)> = expected
            .iter()
            .map(|(prefix, part, text)| ((*prefix).to_string(), *part, *text))
            .collect();
        assert_eq!(statements, expected);
        assert_eq!(
            read.statements[8].line, 35,
            "a statement is placed at its own line"
        );
    }

    #[test]
    fn a_lead_in_does_not_reach_past_a_heading_or_a_paragraph() {
        let read = directive(
            "## 1. One\n\nSupport:\n\n- A.\n\nA paragraph.\n\n- B.\n\nBuild:\n\n- C.\n\n## 2. Two\n\n- D.\n",
        );
        let texts: Vec<&str> = read.statements.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(
            texts,
            ["Support: A.", "A paragraph.", "B.", "Build: C.", "D."]
        );
    }

    #[test]
    fn labels_join_the_paragraph_after_them() {
        let read = directive(
            "## 11. Targets\n\nUse:\n\n**UEFI bootloader**\n\n`x86_64-unknown-uefi`\n\n**Origin userspace**\n\nDo not build it for the kernel's target.\n\n**A bold sentence.**\n\n**M0 — Foundation**\n\n**M1 — Boot**\n\nBoot it.\n",
        );
        let texts: Vec<&str> = read.statements.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(
            texts,
            [
                "Use: **UEFI bootloader** `x86_64-unknown-uefi`",
                "**Origin userspace** Do not build it for the kernel's target.",
                "**A bold sentence.**",
                "**M0 — Foundation**",
                "**M1 — Boot** Boot it.",
            ]
        );
    }

    #[test]
    fn a_line_the_statement_rule_does_not_define_is_refused() {
        let nested = once(
            DIRECTIVE_TEXT,
            "- A loader.\n",
            "- A loader.\n  - Its first stage.\n",
        );
        assert!(directive(&nested).problems[0].contains("line 36 is indented"));
        let mut case = Case::sound();
        case.directive = nested;
        case.refused("CLAUDE.md: line 36 is indented");
        let open_row = once(
            DIRECTIVE_TEXT,
            "| `.ose` | Origin System Executable |",
            "| `.ose` | Origin System Executable",
        );
        assert!(
            directive(&open_row).problems[0].contains("starts a table row and does not end one")
        );
    }

    #[test]
    fn the_decisions_of_an_adr() {
        let read = adr(ADR_TEXT).expect("an ADR");
        assert_eq!((read.number, read.accepted), (1, true));
        let decisions: Vec<(&str, &str, usize)> = read
            .decisions
            .iter()
            .map(|d| (d.id.as_str(), d.title.as_str(), d.line))
            .collect();
        assert_eq!(
            decisions,
            [
                ("K1", "The first", 9),
                ("A1", "Bold title", 17),
                ("Am2", "a single change", 23)
            ]
        );
    }

    #[test]
    fn every_form_of_decision_is_read() {
        let text = "\
# ADR-0042: Forms

| | |
|---|---|
| Status | **Proposed** |

## Decision 1: one container

## Proposed decisions

**H1. What counts.** A platform counts.

**H2. The date scopes claims, not code.**

Not a decision: **H3. Inside a line.**

## Amendment 1 (Accepted, 2026-11-08): what the first slice needs

| # | Question | Decision (implemented) | Alternative |
|---|---|---|---|
| A1 | Which rights? | `READ` and `WRITE`. | None |
| A2 | When does it end? | When its last thread ends. | Never |

## Amendment 2, 2026-11-10: a stricter scope

### B1. The scope (item 88)

### Alternatives

## Amendment 3 (Accepted): the rest

| # | Decision | Refines |
|---|---|---|
| C1 | Default rights. The process's own handle: read. | P1 |
| C2 | `not_found = -14` is appended to the status codes. | P5 |
| C3 | The map call is `a \\| b`, whole | P7 |

## As built

| Decision | Where it is |
|---|---|
| C1 | `rights.rs` |

### Amendment 4: the libraries are outside Z10 (decision item 152)

Its text.

## Approved defaults

The owner accepted both:
1. the umbrella format
2. KRN and SYS profiles

## Threat model

**T1. Not under Decisions.** So not a decision.
";
        let read = adr(text).expect("an ADR");
        assert_eq!((read.number, read.accepted), (42, false));
        let decisions: Vec<(&str, &str)> = read
            .decisions
            .iter()
            .map(|d| (d.id.as_str(), d.title.as_str()))
            .collect();
        assert_eq!(
            decisions,
            [
                ("D1", "one container"),
                ("H1", "What counts"),
                ("H2", "The date scopes claims, not code"),
                ("A1", "Which rights?"),
                ("A2", "When does it end?"),
                ("B1", "The scope (item 88)"),
                ("C1", "Default rights"),
                ("C2", "`not_found = -14` is appended to the status codes"),
                ("C3", "The map call is `a | b`, whole"),
                ("Am4", "the libraries are outside Z10 (decision item 152)"),
                ("def1", "the umbrella format"),
                ("def2", "KRN and SYS profiles"),
            ]
        );
        assert_eq!(
            adr("# Notes\n\n### K1. A heading\n"),
            None,
            "no ADR title, no ADR"
        );
    }

    #[test]
    fn table_cells_round_trip() {
        assert_eq!(
            cells("| a | b \\| c |  |"),
            Some(vec!["a".to_string(), "b | c".to_string(), String::new()])
        );
        assert_eq!(
            cells(&format!("| {} |", escaped("x | y"))),
            Some(vec!["x | y".to_string()])
        );
        assert_eq!(cells("| a | b"), None);
        assert_eq!(cells("no table"), None);
        let found = tables(
            "| A | B |\n|---|:-:|\n| 1 | 2 |\n\n```\n| not | a table |\n```\n| C |\n| 3 | broken\n",
        );
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].rows, [(3, vec!["1".to_string(), "2".to_string()])]);
        assert_eq!(found[1].rows, [(9, vec!["| 3 | broken".to_string()])]);
    }

    #[test]
    fn decision_items_are_numbers_and_ranges() {
        assert_eq!(items("-"), Some(vec![]));
        assert_eq!(items("71–73, 88"), Some(vec![71, 72, 73, 88]));
        assert_eq!(items("5-6"), Some(vec![5, 6]));
        for wrong in ["", "item 5", "5,", "9–7", "1–5000", "five"] {
            assert_eq!(items(wrong), None, "{wrong}");
        }
        assert_eq!(answered(SHEET_TEXT), BTreeSet::from([1, 2, 3]));
    }

    #[test]
    fn a_row_that_does_not_parse_is_refused() {
        let h3 = "| build | both/hw | 2 | not-started |";
        for (to, needle) in [
            (
                "| build | both/hw | - | not-started |",
                "row H-03 has no owner",
            ),
            (
                "| build | both/hw | two | not-started |",
                "row H-03: owner `two` is not a role's number",
            ),
            (
                "| build | both/hw | 2 | finished |",
                "row H-03: state `finished` is not one of",
            ),
            (
                "| make | both/hw | 2 | not-started |",
                "row H-03: kind `make` is not one of",
            ),
            (
                "| build | laptop/hw | 2 | not-started |",
                "row H-03: scope `laptop/hw` is not one of",
            ),
            (
                "| build | both/metal | 2 | not-started |",
                "row H-03: scope `both/metal` is not one of",
            ),
            (
                "| build | both/hw | 2 |",
                "a row has 12 cells, and the matrix has 13 columns",
            ),
            (
                "| info | - | 2 | not-started |",
                "row H-03 asks for nothing, so its state is `-`",
            ),
        ] {
            let case = Case::sound().edit("directive-00-header.md", h3, to);
            case.refused(&format!(
                "docs/requirements/directive-00-header.md:7: {needle}"
            ));
            // A row that is not read leaves its statement without one.
            case.refused("a statement of the header has no row");
        }
        let f1 = "| F-01 | 1. The kernel must boot. | build | both/hw | 2 | not-started | - | - | - | - | none | - | - |";
        for (to, needle) in [
            (f1.replace("| F-01 |", "| F01 |"), "`F01` is not a row's ID"),
            (
                f1.replace("| F-01 |", "| X-01 |"),
                "`X-01` is not a row's ID",
            ),
            (
                f1.replace("1. The kernel must boot.", "-"),
                "row F-01 quotes nothing",
            ),
            (
                f1.replace("| - | none |", "| metal | none |"),
                "env `metal` is not one of",
            ),
            (
                f1.replace("| none |", "| someone |"),
                "review `someone` is not one of",
            ),
            (
                f1.replace("| none |", "| person: |"),
                "review `person:` is not one of",
            ),
            (
                format!(
                    "{}| item 5 |",
                    f1.strip_suffix("| - |").expect("the last cell")
                ),
                "is not `-` or decision items",
            ),
            (
                f1.replace("| not-started | - |", "| not-started | /etc/passwd |"),
                "implementation `/etc/passwd` is not a path in the repository",
            ),
            (
                f1.replace("| not-started | - |", "| not-started | ../other |"),
                "implementation `../other` is not a path in the repository",
            ),
            (
                f1.replace("| - | - | none |", "| a file | - | none |"),
                "evidence `a file` is not",
            ),
            (
                f1.replace("| - | - | none |", "| test:boot/loader.rs | - | none |"),
                "is not `test:<file>::<name>`",
            ),
        ] {
            assert_ne!(to, f1, "{needle}");
            Case::sound()
                .edit("directive-03-final.md", f1, &to)
                .refused(needle);
        }
    }

    #[test]
    fn a_file_that_is_not_a_table_of_the_matrix_is_refused() {
        Case::sound()
            .edit(
                "adr-0001.md",
                "| Limitations | Decided by |",
                "| Limits | Decided by |",
            )
            .refused(
                "docs/requirements/adr-0001.md: the table's header is not `| ID | Requirement |",
            );
        let mut two = Case::sound();
        two.files[0]
            .1
            .push_str("\n| Another | table |\n|---|---|\n");
        two.refused("docs/requirements/directive-00-header.md holds 2 tables");
        let mut stray = Case::sound();
        stray.files.push(("notes.md".to_string(), String::new()));
        stray.refused_once("docs/requirements/notes.md is not a file of the matrix");
        for name in [
            "directive-1-x.md",
            "directive-01.md",
            "adr-1.md",
            "adr-00001.md",
            "adr-0001.txt",
        ] {
            assert_eq!(home(name), None, "{name}");
        }
        assert_eq!(home("directive-07-device-support.md"), Some(Home::Part(7)));
        assert_eq!(home("adr-0017.md"), Some(Home::Adr(17)));
    }

    #[test]
    fn a_run_that_read_no_row_fails() {
        let mut empty = Case::sound();
        empty.files.clear();
        empty.refused("no row was read: nothing was checked");
        let mut headers_only = Case::sound();
        for (_, text) in &mut headers_only.files {
            *text = HEAD.to_string();
        }
        headers_only.refused("no row was read: nothing was checked");
    }

    #[test]
    fn an_owner_that_is_not_a_role_is_refused() {
        Case::sound()
            .edit(
                "directive-00-header.md",
                "| build | both/hw | 2 | not-started |",
                "| build | both/hw | 3 | not-started |",
            )
            .refused_once("row H-03: owner 3 is not one of §1's 2 roles");
    }

    #[test]
    fn an_id_used_twice_is_refused() {
        Case::sound()
            .edit("directive-03-final.md", "| F-02 |", "| F-01 |")
            .refused("directive-03-final.md:6: the ID F-01 is also at directive-03-final.md:5");
    }

    #[test]
    fn evidence_and_implementations_must_be_in_the_tree() {
        let file = "directive-02-build.md";
        for (from, to, needle) in [
            (
                "proof:the loader, test:",
                "proof:the unloader, test:",
                "row S3-01 names evidence that is not in the tree: a boot proof named \"the unloader\"",
            ),
            (
                "::loads",
                "::unloads",
                "row S3-01 names evidence that is not in the tree: a test `unloads` in `boot/loader.rs`",
            ),
            (
                "test:boot/loader.rs::",
                "test:boot/other.rs::",
                "a test `loads` in `boot/other.rs`",
            ),
            (
                "xtask:boot-test",
                "xtask:boot-toast",
                "a `cargo xtask boot-toast` command",
            ),
            (
                "file:rust-toolchain.toml",
                "file:rust-toolchain.tml",
                "row S3-03 names evidence that is not in the tree: the file `rust-toolchain.tml`",
            ),
            (
                "| built | boot/loader.rs |",
                "| built | boot/loader2.rs |",
                "row S3-01: its implementation `boot/loader2.rs` is not in the tree",
            ),
            (
                "| partial | boot |",
                "| partial | boots |",
                "row S3-02: its implementation `boots` is not in the tree",
            ),
        ] {
            Case::sound().edit(file, from, to).refused_once(needle);
        }
        // The proof's name is looked for as the table of proofs writes it, not anywhere.
        let mut renamed = Case::sound();
        renamed
            .tree
            .insert(PROOFS.to_string(), "// the loader\n".to_string());
        renamed.refused("a boot proof named \"the loader\"");
    }

    #[test]
    fn a_state_needs_what_it_claims() {
        let file = "directive-02-build.md";
        for (from, to, needle) in [
            // `done` on evidence weaker than the scope (item 193).
            (
                "| rule | eng/host | 1 | done |",
                "| rule | eng/hw | 1 | done |",
                "row S3-03: it is `done` on evidence that ran on `host`, and its scope asks for `hw`",
            ),
            (
                "| rule | eng/host | 1 | done |",
                "| rule | eng/emu | 1 | done |",
                "its scope asks for `emu`",
            ),
            // Past `not-started` without acceptance criteria (item 195).
            (
                "| The file pins the compiler. |",
                "| - |",
                "row S3-03: it has left `not-started` without acceptance criteria",
            ),
            (
                "| The entry's proof passes. |",
                "| - |",
                "row S3-02: it has left `not-started` without acceptance criteria",
            ),
            (
                "| A gate holds it. |",
                "| - |",
                "row S3-04: it has left `not-started` without acceptance criteria",
            ),
            // Evidence, an implementation and an environment where the state claims them (item 194).
            (
                "| file:rust-toolchain.toml | host |",
                "| - | host |",
                "row S3-03: it names no evidence",
            ),
            (
                "| proof:the loader | emu |",
                "| proof:the loader | - |",
                "row S3-02: it does not say where its evidence ran",
            ),
            (
                "| done | rust-toolchain.toml |",
                "| done | - |",
                "row S3-03: it names no implementation",
            ),
            (
                "| not-verified | docs/x.md |",
                "| not-verified | - |",
                "row S3-04: a row that is not verified names what is in the tree for it",
            ),
            (
                "| A gate holds it. | - | - |",
                "| A gate holds it. | file:docs/x.md | host |",
                "row S3-04: a row with evidence is `partial`, `built` or `done`",
            ),
            (
                "| QEMU only. |",
                "| - |",
                "row S3-01: a `partial` or `built` row says in its limitations",
            ),
            (
                "| No entry on a second processor. |",
                "| - |",
                "row S3-02: a `partial` or `built` row says in its limitations",
            ),
            // Hardware is claimed with a record of the run.
            (
                "xtask:boot-test | emu |",
                "xtask:boot-test | hw |",
                "row S3-01: its evidence ran on hardware, and it names no record under docs/hardware/",
            ),
        ] {
            Case::sound().edit(file, from, to).refused_once(needle);
        }
        let not_started = "| F-01 | 1. The kernel must boot. | build | both/hw | 2 | not-started | - | - | - | - | none | - | - |";
        for to in [
            not_started.replace("| not-started | - |", "| not-started | boot |"),
            not_started.replace("| - | - | none |", "| file:docs/x.md | - | none |"),
            not_started.replace("| - | none |", "| host | none |"),
            not_started.replace("| none |", "| agent |"),
        ] {
            assert_ne!(to, not_started);
            Case::sound()
                .edit("directive-03-final.md", not_started, &to)
                .refused_once("row F-01: a row that is not started has no implementation, evidence, environment or review");
        }
    }

    #[test]
    fn evidence_at_or_above_the_scope_can_be_done() {
        const RECORD: &str = "docs/hardware/records/first-machine.md";
        let record = "# The first machine\n\n| | |\n|---|---|\n| Machine | A board |\n| Firmware | 1.2 |\n| Source revision | 00dc7aa |\n| Date | 2027-01-01 |\n| Result | `boot-matrix` passed |\n";
        let on_hardware = |evidence: &str| {
            let mut case = Case::sound()
                .edit(
                    "directive-02-build.md",
                    "| build | both/hw | 2 | built |",
                    "| build | both/hw | 2 | done |",
                )
                .edit(
                    "directive-02-build.md",
                    "xtask:boot-test | emu |",
                    &format!("xtask:boot-test, {evidence} | hw |"),
                );
            case.readme = once(README_TEXT, "| done | S3-03 |", "| done | S3-03, S3-01 |");
            case
        };
        // The record is named and is not in the tree.
        let mut case = on_hardware(&format!("file:{RECORD}"));
        case.refused_once(&format!("the file `{RECORD}`"));
        // It is there, and says what a record says.
        case.tree.insert(RECORD.to_string(), record.to_string());
        assert_eq!(case.judged().0, Vec::<String>::new());
        // It leaves two things out, one of them by a dash.
        case.tree.insert(
            RECORD.to_string(),
            record
                .replace("| Firmware | 1.2 |\n", "")
                .replace("| 2027-01-01 |", "| - |"),
        );
        case.refused_once(&format!(
            "row S3-01: its hardware record `{RECORD}` does not say: Firmware, Date. A record is a table with a row for each of Machine, Firmware, Source revision, Date, Result"
        ));
        // Another file under docs/hardware/ is no record: the list of machines to buy is there.
        let mut shortlist = on_hardware("file:docs/hardware/SHORTLIST.md");
        shortlist
            .tree
            .insert("docs/hardware/SHORTLIST.md".to_string(), record.to_string());
        shortlist.refused_once(
            "row S3-01: its evidence ran on hardware, and it names no record under docs/hardware/records/",
        );
        // Emulation is enough for a row whose scope is the host.
        let above = Case::sound().edit(
            "directive-02-build.md",
            "| file:rust-toolchain.toml | host |",
            "| file:rust-toolchain.toml | emu |",
        );
        assert_eq!(above.judged().0, Vec::<String>::new());
    }

    #[test]
    fn headings_and_code_blocks_are_held_by_a_digest() {
        // What a row quotes does not move a digest: the rows hold that.
        let reworded = once(DIRECTIVE_TEXT, "- A kernel entry.", "- A kernel exit.");
        let renumbered = once(
            DIRECTIVE_TEXT,
            "| `.ose` | Origin System Executable |",
            "| `.ose` | Executable |",
        );
        assert_eq!(unquoted(&reworded), unquoted(DIRECTIVE_TEXT));
        assert_eq!(unquoted(&renumbered), unquoted(DIRECTIVE_TEXT));
        assert_eq!(
            unquoted(DIRECTIVE_TEXT)
                .keys()
                .copied()
                .collect::<Vec<u8>>(),
            [0, 1, 2, 3]
        );
        // What no row quotes does, in its own PART.
        for (from, to, part) in [
            ("channel = \"1.99.1\"", "channel = \"1.99.2\"", 2),
            ("```toml", "```text", 2),
            ("## 3. Boot", "## 3. Booting", 2),
            ("# PART II — BUILD", "# PART II — BUILDING", 2),
            ("| Extension | Purpose |", "| Extension | Use |", 0),
            ("## 2. Priorities", "## 2. The priorities", 1),
            (
                "# FINAL MANDATORY ENGINEERING RULES",
                "# FINAL MANDATORY ENGINEERING RULES, AGAIN",
                3,
            ),
        ] {
            let mut case = Case::sound();
            case.recorded_from = Some(DIRECTIVE_TEXT.to_string());
            case.directive = once(DIRECTIVE_TEXT, from, to);
            case.refused_once(&format!(
                "CLAUDE.md: the headings, code blocks or table headers of PART {part:02} are not as docs/requirements/README.md records them"
            ));
        }
        // A PART the README has no digest for, and one the directive does not have.
        let mut gained = Case::sound();
        gained.recorded_from = Some(DIRECTIVE_TEXT.to_string());
        gained.directive = once(
            DIRECTIVE_TEXT,
            "# FINAL MANDATORY",
            "# PART III — MORE\n\n# FINAL MANDATORY",
        );
        gained.refused(
            "PART 04 are not as docs/requirements/README.md records them (they give SHA-256",
        );
        gained.refused("and the README has none)");
        let mut lost = Case::sound();
        lost.recorded_from = Some(once(
            DIRECTIVE_TEXT,
            "# FINAL MANDATORY",
            "# PART III — MORE\n\n# FINAL MANDATORY",
        ));
        lost.refused("README.md records a digest for PART 04, which CLAUDE.md does not have");
        let mut unrecorded = Case::sound();
        unrecorded.recorded_from = Some(String::new());
        unrecorded.refused_once("README.md has no table of what the rows do not quote");
        let mut twice = Case::sound();
        twice.readme.push_str("\n| Part | Headings and code blocks |\n|---|---|\n| 09 | abc |\n| 09 | abc |\n| nine | abc |\n");
        twice.refused("README.md:24: PART 09 is recorded twice");
        twice.refused(
            "README.md:25: a row of what the rows do not quote is a PART's number and a digest",
        );
    }

    #[test]
    fn the_rows_that_move_the_figure_are_listed() {
        for (from, to, needle) in [
            // A row that is done, or asks for nothing, and is not on its list.
            (
                "| done | S3-03 |\n",
                "| done | - |\n",
                "directive-02-build.md:8: row S3-03 is `done`, and the README's list of `done` rows does not name it",
            ),
            (
                "| info | P2-01, F-02 |",
                "| info | P2-01 |",
                "directive-03-final.md:6: row F-02 is `info`, and the README's list of `info` rows does not name it",
            ),
            // A row on a list that is not what the list says, or is no row.
            (
                "| done | S3-03 |",
                "| done | S3-03, S3-01 |",
                "README.md lists S3-01 as `done`, and the matrix has no such row that is",
            ),
            (
                "| info | P2-01, F-02 |",
                "| info | P2-01, F-02, F-01 |",
                "README.md lists F-01 as `info`, and the matrix has no such row that is",
            ),
            (
                "| info | P2-01, F-02 |",
                "| info | P2-01, F-02, F-09 |",
                "README.md lists F-09 as `info`, and the matrix has no such row that is",
            ),
            (
                "| info | P2-01, F-02 |",
                "| info | P2-01, F-02, H-01 |",
                "README.md:19: the list names H-01 twice",
            ),
            (
                "| done | S3-03 |",
                "| built | S3-01 |",
                "README.md:17: a list of rows is `done` or `info`, and then the rows' IDs",
            ),
        ] {
            let mut case = Case::sound();
            case.readme = once(README_TEXT, from, to);
            case.refused(needle);
        }
        // A row cannot leave the count, or become done, by its own cells alone.
        Case::sound()
            .edit(
                "directive-00-header.md",
                "| build | both/hw | 2 | not-started |",
                "| info | - | 2 | - |",
            )
            .refused_once(
                "row H-03 is `info`, and the README's list of `info` rows does not name it",
            );
        Case::sound()
            .edit(
                "directive-01-authority.md",
                "| S1-02 | 1. **First Architect.** Own the first things. | build | eng/host | 1 | not-started | - | - | - | - | none |",
                "| S1-02 | 1. **First Architect.** Own the first things. | build | eng/host | 1 | done | docs/x.md | It is there. | file:docs/x.md | host | none |",
            )
            .refused_once("row S1-02 is `done`, and the README's list of `done` rows does not name it");
    }

    #[test]
    fn the_commonest_criterion_is_counted() {
        let (found, figure) = Case::sound()
            .edit(
                "directive-02-build.md",
                "| The entry's proof passes. |",
                "| A gate holds it. |",
            )
            .judged();
        assert_eq!(found, Vec::<String>::new());
        assert_eq!((figure.criteria_texts, figure.criteria_commonest), (3, 2));
    }

    #[test]
    fn every_statement_has_a_row_and_every_row_a_statement() {
        // A statement with no row: the message gives the row's first cells.
        let row = "| S3-02 | Implement: A kernel entry. | build | both/hw | 2 | partial | boot | The entry's proof passes. | proof:the loader | emu | none | No entry on a second processor. | - |\n";
        Case::sound()
            .edit("directive-02-build.md", row, "")
            .refused_once("CLAUDE.md:36: a statement of §3 has no row. Its row starts: | S3-06 | Implement: A kernel entry. |");
        // A statement that changed: its row is stale, and the new text has none.
        let mut changed = Case::sound();
        changed.directive = once(DIRECTIVE_TEXT, "- A kernel entry.", "- A kernel exit.");
        let (found, _) = changed.judged();
        assert_eq!(found.len(), 2, "{found:#?}");
        changed.refused("directive-02-build.md:7: row S3-02 quotes a statement that CLAUDE.md does not make there");
        changed.refused("Its row starts: | S3-06 | Implement: A kernel exit. |");
        // The same words in another section are another statement.
        Case::sound()
            .edit(
                "directive-02-build.md",
                "| S3-05 | Do not stop. | rule | eng/host | 1 | removed |",
                "| S3-05 | Memory safety. | rule | eng/host | 1 | not-started |",
            )
            .refused("row S3-05 quotes a statement that CLAUDE.md does not make there");
        // A new section's statement is offered its first ID.
        let mut added = Case::sound();
        added.directive = once(
            DIRECTIVE_TEXT,
            "# FINAL",
            "## 4. More\n\nBuild a `a | b` switch.\n\n# FINAL",
        );
        added.refused_once(
            "a statement of §4 has no row. Its row starts: | S4-01 | Build a `a \\| b` switch. |",
        );
    }

    #[test]
    fn two_statements_with_the_same_words_need_two_rows() {
        let mut twice = Case::sound().edit(
            "directive-02-build.md",
            "| S3-02 | Implement: A kernel entry. |",
            "| S3-02 | Implement: A loader. |",
        );
        twice.directive = once(DIRECTIVE_TEXT, "- A kernel entry.", "- A loader.");
        assert_eq!(twice.judged().0, Vec::<String>::new());
        let row = "| S3-02 | Implement: A loader. | build | both/hw | 2 | partial | boot | The entry's proof passes. | proof:the loader | emu | none | No entry on a second processor. | - |\n";
        twice
            .edit("directive-02-build.md", row, "")
            .refused_once("a statement of §3 has no row");
    }

    #[test]
    fn a_removed_row_may_outlive_its_statement() {
        let mut gone = Case::sound();
        gone.directive = once(DIRECTIVE_TEXT, "Do not stop.\n\n", "");
        assert_eq!(gone.judged().0, Vec::<String>::new());
    }

    #[test]
    fn a_removed_row_names_its_item_and_a_scope_change_names_the_row() {
        // Without its item: and the item is then named by no row.
        let case = Case::sound().edit(
            "directive-02-build.md",
            "| Taken out of scope. | 2 |",
            "| Taken out of scope. | - |",
        );
        case.refused("row S3-05: a removed row names the decision item that took it out of scope");
        case.refused("answered decision items that no row names and the README does not list: 2.");
        // With no scope change that names it.
        let mut unrecorded = Case::sound();
        unrecorded.readme = once(
            README_TEXT,
            "| 2 | 2026-01-01 | Stopping is allowed. | S3-05 |\n",
            "",
        );
        unrecorded.refused_once("row S3-05 is removed, and no scope change in the README names it with its decision item");
        // With a scope change for another item.
        let mut other = Case::sound();
        other.readme = once(README_TEXT, "| 2 | 2026-01-01 |", "| 1 | 2026-01-01 |");
        other.refused_once("row S3-05 is removed, and no scope change in the README names it with its decision item");
        // A scope change may name its rows by what their IDs start with.
        let mut by_prefix = Case::sound();
        by_prefix.readme = once(README_TEXT, "| S3-05 |", "| A01-*, S3-* |");
        assert_eq!(by_prefix.judged().0, Vec::<String>::new());
    }

    #[test]
    fn a_scope_change_names_answered_items_and_rows_that_exist() {
        for (from, to, needle) in [
            (
                "| S3-05 |",
                "| S3-05, S3-99 |",
                "README.md:9: a scope change names `S3-99`, and the matrix has no such row",
            ),
            (
                "| S3-05 |",
                "| S3-05, S9-* |",
                "a scope change names `S9-*`, and the matrix has no such row",
            ),
            (
                "| 2 | 2026-01-01 |",
                "| 2, 4 | 2026-01-01 |",
                "a scope change names decision item 4, which docs/engineering/OWNER_DECISIONS.md does not hold as answered",
            ),
            (
                "| 2 | 2026-01-01 | Stopping is allowed. | S3-05 |",
                "| 2 | 2026-01-01 | Stopping is allowed. |",
                "README.md:9: a scope change is four cells",
            ),
            (
                "| 2 | 2026-01-01 |",
                "| item 2 | 2026-01-01 |",
                "README.md:9: a scope change is four cells",
            ),
        ] {
            let mut case = Case::sound();
            case.readme = once(README_TEXT, from, to);
            case.refused(needle);
        }
        let mut no_rows = Case::sound();
        no_rows.readme = once(README_TEXT, "| S3-05 |", "| - |");
        no_rows.refused("README.md:9: a scope change names no row");
    }

    #[test]
    fn rows_stay_in_the_file_and_the_order_of_their_source() {
        let f1 = "| F-01 | 1. The kernel must boot. | build | both/hw | 2 | not-started | - | - | - | - | none | - | - |\n";
        let mut moved = Case::sound().edit("directive-03-final.md", f1, "");
        moved.files[2].1.push_str(f1);
        moved.refused_once("directive-02-build.md:11: row F-01 belongs in the file of PART 3, directive-03-<name>.md");
        let k1 = "| A01-K1 | The first | build | both/hw | 2 | not-started | - | - | - | - | none | - | 1 |\n";
        let mut adr_row = Case::sound().edit("adr-0001.md", k1, "");
        adr_row.files[3].1.push_str(k1);
        adr_row.refused_once("directive-03-final.md:7: row A01-K1 belongs in adr-0001.md");
        // Two rows exchanged.
        let first = "| P2-01 | The formats are profiles. | info | - | 2 | - | - | - | - | - | none | - | - |\n";
        let mut swapped = Case::sound().edit("directive-02-build.md", first, "");
        swapped.files[2].1.push_str(first);
        swapped.refused_once("directive-02-build.md:10: row P2-01 is out of its source's order: it quotes line 29, and the row before it line 48");
    }

    #[test]
    fn every_decision_of_an_accepted_adr_has_a_row_that_quotes_it() {
        let a1 = "| A01-A1 | Bold title | build | both/hw | 2 | not-started | - | - | - | - | none | - | - |\n";
        Case::sound().edit("adr-0001.md", a1, "").refused_once(
            "ADR-0001's decision A1 (line 17) has no row. Its row starts: | A01-A1 | Bold title |",
        );
        Case::sound()
            .edit("adr-0001.md", "| Bold title |", "| Bald title |")
            .refused_once("adr-0001.md:6: row A01-A1 does not quote ADR-0001's A1 as the ADR has it (line 17): Bold title");
        let renamed = Case::sound().edit("adr-0001.md", "| A01-A1 |", "| A01-A9 |");
        renamed.refused("row A01-A9: ADR-0001 has no decision A9");
        renamed.refused("ADR-0001's decision A1 (line 17) has no row");
        // A decision the ADR gains needs a row.
        let mut gained = Case::sound();
        gained.adrs[0]
            .1
            .push_str("\n## Amendment 3 (Accepted): another\n\nText.\n");
        gained.refused_once(
            "ADR-0001's decision Am3 (line 27) has no row. Its row starts: | A01-Am3 | another |",
        );
        // An ADR that is not accepted is not a source, and a row for it is refused.
        let mut proposed = Case::sound();
        proposed.adrs[0].1 = once(ADR_TEXT, "**Accepted** by the owner", "**Proposed**");
        let (found, figure) = proposed.judged();
        assert_eq!((figure.adrs, figure.decisions), (0, 0));
        assert_eq!(
            found
                .iter()
                .filter(|p| p.contains("is a row for ADR-0001, which is not accepted"))
                .count(),
            3,
            "{found:#?}"
        );
        // A file under docs/adr that is no ADR is said, not skipped.
        let mut untitled = Case::sound();
        untitled
            .adrs
            .push(("ADR-0002-notes.md".to_string(), "Notes.\n".to_string()));
        untitled.refused_once("docs/adr/ADR-0002-notes.md: no `# ADR-NNNN` title was read");
    }

    #[test]
    fn every_answered_item_is_named_by_a_row_or_listed() {
        // Named by no row and not listed.
        Case::sound()
            .edit("adr-0001.md", "| - | 1 |", "| - | - |")
            .edit(
                "directive-02-build.md",
                "| person:Ada | - | 1 |",
                "| person:Ada | - | - |",
            )
            .refused_once(
                "answered decision items that no row names and the README does not list: 1.",
            );
        // A row names an item the sheet does not hold as answered.
        Case::sound()
            .edit("adr-0001.md", "| - | 1 |", "| - | 1, 4 |")
            .refused_once("adr-0001.md:5: row A01-K1 names decision item 4, which docs/engineering/OWNER_DECISIONS.md does not hold as answered");
        for (from, to, needle) in [
            (
                "| 3 | none |",
                "| 1, 3 | none |",
                "README.md:13: decision item 1 is listed without rows, and a row names it",
            ),
            (
                "| 3 | none |",
                "| 3, 4 | none |",
                "README.md:13: decision item 4 is listed without rows, and docs/engineering/OWNER_DECISIONS.md does not hold it as answered",
            ),
            (
                "| 3 | none |",
                "| 3, 3 | none |",
                "README.md:13: decision item 3 is listed twice",
            ),
            (
                "| 3 | none |",
                "| 3 | later |",
                "README.md:13: an entry of the items without rows is three cells",
            ),
            (
                "| 3 | none | A process rule. |\n",
                "",
                "answered decision items that no row names and the README does not list: 3.",
            ),
        ] {
            let mut case = Case::sound();
            case.readme = once(README_TEXT, from, to);
            case.refused(needle);
        }
        // Rows owed are counted apart from items that ask for nothing.
        let mut owed = Case::sound();
        owed.readme = once(README_TEXT, "| 3 | none |", "| 3 | owed |");
        let (found, figure) = owed.judged();
        assert_eq!(found, Vec::<String>::new());
        assert_eq!((figure.items_nothing, figure.items_owed), (0, 1));
    }

    #[test]
    fn unnamed_items_are_one_problem() {
        let mut case = Case::sound()
            .edit("adr-0001.md", "| - | 1 |", "| - | - |")
            .edit(
                "directive-02-build.md",
                "| person:Ada | - | 1 |",
                "| person:Ada | - | - |",
            );
        case.readme = once(README_TEXT, "| 3 | none | A process rule. |\n", "");
        case.refused_once(
            "answered decision items that no row names and the README does not list: 1, 3.",
        );
    }

    /// The matrix in this checkout: what `cargo xtask check` runs, as a test of its own.
    #[test]
    fn the_matrix_in_this_tree_passes() {
        let read = read().expect("the matrix and its sources");
        let sources = Sources {
            directive: &read.directive,
            adrs: &read.adrs,
            sheet: &read.sheet,
            readme: &read.readme,
            files: &read.files,
        };
        let (found, figure) = problems(&sources, &Disk(repo_root()));
        assert_eq!(found, Vec::<String>::new());
        assert_eq!(figure.rows, figure.statements + figure.decisions);
        assert!(
            figure.statements > 1000 && figure.decisions > 200,
            "{figure:?}"
        );
        // Nothing has run on a physical machine, and no person has reviewed anything: when
        // either changes, this line changes with the record that shows it.
        assert_eq!(
            (figure.ran[Env::Hardware as usize], figure.by_person),
            (0, 0)
        );
    }

    #[test]
    fn the_matrix_is_checked_against_the_directives_revision() {
        let mut older = Case::sound();
        older.readme = once(
            README_TEXT,
            "| Directive revision | 9.1 |",
            "| Directive revision | 9.0 |",
        );
        older.refused_once("README.md says the matrix was checked against revision 9.0, and CLAUDE.md is revision 9.1");
        let mut unsaid = Case::sound();
        unsaid.readme = once(README_TEXT, "| Directive revision | 9.1 |\n", "");
        unsaid.refused_once("README.md: no `| Directive revision | … |` row");
        let mut unversioned = Case::sound();
        unversioned.directive = once(
            DIRECTIVE_TEXT,
            "**Revision:** 9.1, 2026-01-01. Revision 9.0 said less.  \n",
            "",
        );
        unversioned.refused("CLAUDE.md: no `**Revision:**` line was read");
    }

    #[test]
    fn a_readme_without_its_tables_is_refused() {
        let mut bare = Case::sound();
        bare.readme = "| | |\n|---|---|\n| Directive revision | 9.1 |\n".to_string();
        bare.refused("README.md has no table of scope changes");
        bare.refused("README.md has no table of decision items without rows");
        bare.refused("README.md has no lists of the rows that are `done` and that are `info`");
    }

    #[test]
    fn sources_that_cannot_be_read_fail() {
        let mut no_directive = Case::sound();
        no_directive.directive = String::new();
        no_directive.refused("CLAUDE.md: no statement was read");
        no_directive.refused("CLAUDE.md: no role was read from §1");
        let mut no_sheet = Case::sound();
        no_sheet.sheet = String::new();
        no_sheet.refused("docs/engineering/OWNER_DECISIONS.md: no answered item was read");
    }

    #[test]
    fn many_problems_of_one_kind_are_listed_in_part() {
        let mut case = Case::sound();
        let mut more = String::from("\n## 4. More\n\n");
        for n in 0..40 {
            more.push_str(&format!("- Thing {n}.\n"));
        }
        case.directive = once(DIRECTIVE_TEXT, "\n# FINAL", &format!("{more}\n# FINAL"));
        let (found, _) = case.judged();
        assert_eq!(found.len(), LISTED + 1, "{found:#?}");
        assert_eq!(found[LISTED], "… and 15 more of the same kind");
    }

    #[test]
    fn a_percentage_has_one_decimal() {
        assert_eq!(percent(0, 0), "0.0%");
        assert_eq!(percent(1, 12), "8.3%");
        assert_eq!(percent(12, 12), "100.0%");
        assert_eq!(percent(1, 1603), "0.1%");
    }
}
