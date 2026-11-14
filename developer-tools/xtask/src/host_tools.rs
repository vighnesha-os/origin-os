//! `cargo xtask host-tools-gate`: the host tools are libraries first (ADR-0011 Amendment 4, T1;
//! decision items 295 and 300; TASKS item 60). Part of `check`. Three halves and a check of
//! manifests, read with xtask's lexer (`lex.rs`), so comments and the text inside string
//! literals are not code. Its rules were tightened after the independent review of 2026-11-14
//! (`docs/engineering/REVIEW_T60_2026-11-14.md`, findings F1 to F5, F8, F11 and F12, and its
//! addendum's N1 to N6).
//!
//! **G1: `xtask` starts no first-party program.** It calls `onbf-pack`'s and `oidl`'s libraries
//! (and `onbf-sign`'s through `onbf_pack::sign`); the programs it starts are outside ones:
//! Cargo, `rustc`, Git, `llvm-objdump`, `nm`, QEMU. Over every `.rs` file of
//! `developer-tools/xtask/`, its tests included, the gate refuses:
//!
//! - **Cargo's `run`, and the aliases of the repository root's Cargo configuration that run a
//!   program**: in the parentheses of an `.arg(…)` or `.args(…)` call, a string literal, or a
//!   `const NAME: … = "…";` defined in `xtask`, whose first word is `run`, Cargo's built-in alias
//!   `r`, or an alias of `<root>/.cargo/config.toml` or `<root>/.cargo/config` that runs a
//!   program (`[alias]` and `alias.<name>` keys, followed alias to alias: today `xtask`). An
//!   alias's name is not refused when it is a whole element of the list and the element before it
//!   is a literal option that takes a value (`["test", "--package", "xtask"]`); an element that is
//!   not a literal exempts nothing. The alias `xtask` is allowed where `xtask` may re-run itself
//!   ([`SELF_RUNS`]). A literal there that defines an alias (`alias.…`, `--config=alias.…`) is
//!   refused, and a literal beginning `CARGO_ALIAS_` is refused anywhere in `xtask`. It also
//!   refuses `run` given to an outside program; none takes it today.
//! - **A first-party program by name or path**: inside the parentheses of `Command::new(…)`, a
//!   literal (or such a `const`) that is the name of a workspace package or of a `[[bin]]`
//!   target, alone or as a path's last component. The names are read from every manifest in the
//!   tree on each run.
//! - **`xtask` naming itself** (`current_exe`) outside the functions [`SELF_RUNS`] lists.
//!
//! What it allows, each named here and in [`SELF_RUNS`] with its authority:
//!
//! - **By decision item 300** (accepted by the owner on 2026-11-14; T1's added note in
//!   ADR-0011 Amendment 4): `cargo xtask exclusive` running the command a person gives it
//!   (`turns::exclusive`), that person's choice and not `xtask`'s, whose argument is their text
//!   so neither rule meets it; and `boot-matrix` re-running `xtask` for each scenario
//!   (`boot_matrix`, through `boot_test_child`, with each scenario's `ORIGIN_QEMU_*` settings in
//!   its child's environment), until TASKS item 61 runs the scenarios in one process with typed
//!   settings, after TASKS items 58 and 59.
//! - **By decision item 310** (accepted by the owner on 2026-11-14, "Accept 310"; T1's second
//!   note in ADR-0011 Amendment 4; the review's finding F6): item 300's allowance covers the six
//!   scenario commands `boot-matrix` runs as steps of its own (`boot_x2apic`, `boot_bridge`,
//!   `boot_reserved`, `boot_legacy_free`, `boot_random`, `boot_identity`), also when a person
//!   runs one on its own, since each re-runs `xtask` the same way, until TASKS item 61. And a
//!   test that starts a first-party program to test that same program is outside T1: the turn
//!   lock's own test (`turns::tests::the_lock_goes_with_the_process_however_it_ends`) starts its
//!   test binary to hold the lock and be killed. It stays listed in [`SELF_RUNS`], so that every
//!   exception the gate allows is named.
//! - **Outside G1, by decision item 310** (finding F12): Cargo's subcommands other than `run`
//!   (`xtask check` runs `cargo test --package onbf-pack`, and `onbf-pack`'s and `oidl`'s
//!   integration tests start the binary they test, which item 310 puts outside T1 too) and
//!   first-party build scripts are outside programs here, as Cargo is.
//!
//! **G2: the three libraries reach nothing of the host, by the forms it reads.** Over the
//! library sources of `onbf-pack`, `onbf-sign` and `oidl` (`src/**/*.rs`, but not
//! `src/main.rs`, `src/tests.rs`, `src/tests/` or an item under `#[cfg(test)]`, which ends at
//! its `;` or `,`, with its block, or before the bracket that closes its list: so a test-only
//! field, variant or match arm hides nothing after it), the gate refuses:
//!
//! - a path through `std::fs`, `std::process`, `std::env`, `std::net`, `std::os`, `std::time`
//!   or `std::path` (whose `Path` reads the file system), written out (`::std::` too) or as a
//!   name in a `use std::{…}` group;
//! - the identifiers `stdin`, `stdout` and `stderr`, wherever they stand, so a stream reached
//!   through `use std::io::{self, …}` or a renamed import is caught too;
//! - a glob import from `std` (`use std::*`, `use std::fs::*`, `use std::io::*`);
//! - a string literal that begins `/dev/`;
//! - `print!`, `println!`, `eprint!`, `eprintln!` and `dbg!`: a library returns what it has to
//!   say.
//!
//! Compile-time inclusion (`include_str!` of the generator's templates) is not run-time host
//! access, and passes.
//!
//! **The libraries' dependencies.** Each library's manifest may name, in its normal, build and
//! target-specific dependencies, only the crates [`LIBRARY_DEPENDENCIES`] lists for it:
//! first-party `no_std` crates that ship in the loader or the kernel, and `onbf-sign`, which G2
//! reads itself. G2 reads the libraries' own sources only, so this keeps what they depend on in
//! view.
//!
//! **G3: each command line names no crate but its library's and the standard library's.** In
//! `onbf-pack`'s and `oidl`'s `src/main.rs`, an identifier that names a first-party crate or one
//! of the tool's dependencies, other than its own library, and an `extern crate` of anything but
//! `std`, `core`, `alloc` and the library, is refused. That a command line holds no logic of its
//! own, and reads, calls and writes only, is review's to see, not this gate's.
//!
//! **Negative controls.** Every run, before it reports, the gate checks planted copies of real
//! files ([`controls`]), each of which must be refused or the gate fails: in `xtask`'s `main.rs`,
//! the `cargo run --release --package onbf-pack` that `xtask` ran before T1, a `Command::new` of
//! `oidl`'s binary, a `current_exe` outside the list, `cargo().args(["xtask", "boot-test"])`
//! outside it, and the same after a `--config` whose value is a variable; in `onbf-sign`'s
//! `lib.rs`, the `os_random` it had before T1, a test-only field with host access after it (which
//! the scan before the fix for F1 would miss), a stream through `std::io` and a `Path::is_file`;
//! in `onbf-pack`'s `main.rs`, the `onbf_verify::key_id` call it had before T1; in `onbf-sign`'s
//! manifest, a dependency on `onbf-pack`. The host tests plant every refused form.
//!
//! **What it cannot see** (the review's findings F8 and F12, its addendum's N2, N5 and N6, and
//! before them):
//!
//! - It reads tokens, not data flow. An argument list or a program's path built in a variable
//!   somewhere else and handed to `.args(…)` or `Command::new(…)` is not seen; nor a `static` or a
//!   `const` of another type, one defined outside `xtask`, or one of two `const`s with one name
//!   in two files (the last read wins); nor `Command::arg(&mut c, "run")` or a turbofish
//!   (`.args::<_, &str>([…])`) or `use std::process::Command as Cmd; Cmd::new(…)`.
//! - `xtask` naming itself other than by `current_exe` (`env::args_os().next()`,
//!   `/proc/self/exe`).
//! - A binary found by Cargo as `src/bin/<name>.rs`, or a manifest `name` line with a comment
//!   after it (none in the tree).
//! - Raw identifiers (`r#std::fs`, `r#current_exe`, `Command::r#new`, `r#println!`), which the
//!   lexer keeps apart from identifiers; `extern crate std as host;` and `use std as s;`; and a
//!   string literal holding an escape the lexer does not decode (a `\` line continuation), which
//!   is skipped, not refused.
//! - A Cargo alias defined in the environment `xtask` inherits (`CARGO_ALIAS_<NAME>`) or in a
//!   Cargo configuration outside the repository (a parent directory, Cargo's home), which Cargo
//!   reads and a lexical gate does not; and an alias's name or definition, or a `CARGO_ALIAS_…`
//!   name, built with `concat!`, `format!` or at run time, as this gate builds its own prefix
//!   (the review's addendum, finding N2).
//! - Controls that can fail with nothing broken, a false failure and never a false pass (finding
//!   N5): the two alias controls need the root's configuration to keep `xtask` as an alias that
//!   runs, and the manifest control needs `onbf-sign`'s manifest to hold the line `[dependencies]`
//!   exactly, with nothing after it and Unix line endings.
//! - False refusals, each failing closed (finding N6): a comma inside `<…>` ends a test-only item
//!   early, so the rest of it is read as library code; G3 refuses a local identifier equal to a
//!   crate's name (`first_program`, `origin_core`, `xtask`); `.arg("-p").arg("xtask")` is refused,
//!   since an option's value is read within one argument list only; and an editor's backup with an
//!   owned name (`<name>.rs~`) that cannot be read stops `carried`.
//! - A library that reaches the host through a crate it depends on: the dependencies are held
//!   to [`LIBRARY_DEPENDENCIES`], whose crates are read by their own gates, not this one.
//!
//! Review covers what the gate does not (`docs/engineering/SOURCE_PROVENANCE.md`, TASKS item 60).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::lex::{self, Kind, Token};
use crate::{Result, collect_files, read_file, repo_root};

/// One refusal in a file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Hit {
    /// The line, counted from 1.
    pub(crate) line: usize,
    /// What was refused, and why.
    pub(crate) what: String,
}

/// Cargo's subcommand that runs a package's binary, and its built-in alias. The aliases the
/// repository's Cargo configuration defines are added on each run ([`run_aliases`]).
const RUN_WORDS: &[&str] = &["run", "r"];

/// The prefix of the environment variables that define a Cargo alias for one command. Spelled in
/// two pieces so that no literal of this file is itself refused.
const ALIAS_VARIABLE: &str = concat!("CARGO_", "ALIAS_");

/// The options of Cargo that take a value as the next argument: an alias's name after one of
/// these is that value (`--package xtask`), not the alias.
const VALUE_OPTIONS: &[&str] = &[
    "--package",
    "-p",
    "--bin",
    "--example",
    "--test",
    "--bench",
    "--manifest-path",
    "--target-dir",
    "--target",
    "--features",
    "-F",
    "--config",
    "--profile",
    "--message-format",
    "--jobs",
    "-j",
    "--color",
    "-Z",
];

/// What G1 knows of the tree: the first-party programs' names, and the words that make Cargo
/// run a program, each with the package it runs when the words say (`None` for `run` itself).
#[derive(Clone, Debug, Default)]
pub(crate) struct Known {
    /// Every workspace package's name and every `[[bin]]` target's.
    pub(crate) programs: BTreeSet<String>,
    /// `run`, `r`, and every alias of the repository's Cargo configuration whose expansion
    /// runs a program, with the package it names.
    pub(crate) runs: BTreeMap<String, Option<String>>,
}

/// Item 300's words: `boot-matrix` re-running `xtask` for each scenario.
const ITEM_300: &str = "decision item 300: `boot-matrix` re-runs `xtask` for each scenario";
/// The six scenario commands `boot-matrix` runs as steps, also when run alone (the review's
/// finding F6; decision item 310).
const ITEM_310_MACHINE: &str = "decision item 310: a scenario command of `boot-matrix`'s, also when run on its own, until TASKS item 61";
/// The turn lock's own test (finding F6; decision item 310).
const ITEM_310_TEST: &str = "decision item 310: a test starting the program it tests, outside T1";

/// The places `xtask` may name itself (`current_exe`), by file under `developer-tools/xtask/`
/// and function, each with its authority (the module documentation says more). A place not
/// listed is refused, and a listed one that no longer names itself is a stale allowance, refused
/// too.
pub(crate) const SELF_RUNS: &[(&str, &str, &str)] = &[
    ("src/main.rs", "boot_matrix", ITEM_300),
    ("src/main.rs", "boot_reserved", ITEM_310_MACHINE),
    ("src/main.rs", "boot_legacy_free", ITEM_310_MACHINE),
    ("src/main.rs", "boot_random", ITEM_310_MACHINE),
    ("src/main.rs", "boot_identity", ITEM_310_MACHINE),
    ("src/main.rs", "boot_bridge", ITEM_310_MACHINE),
    ("src/main.rs", "boot_x2apic", ITEM_310_MACHINE),
    (
        "src/turns.rs",
        "the_lock_goes_with_the_process_however_it_ends",
        ITEM_310_TEST,
    ),
];

/// Whether `function` of `file` is a place [`SELF_RUNS`] lists.
fn self_run(file: &str, function: &str) -> bool {
    SELF_RUNS
        .iter()
        .any(|(f, name, _)| *f == file && *name == function)
}

/// The three host tools whose libraries G2 reads, by directory.
const LIBRARIES: &[&str] = &[
    "developer-tools/onbf-pack",
    "developer-tools/onbf-sign",
    "developer-tools/oidl",
];

/// Each library's dependencies, as its manifest may name them (the review of 2026-11-14, finding
/// F11): first-party crates only, each a `no_std` library that ships in the loader or the kernel,
/// or `onbf-sign`, which G2 reads itself. A library that gains any other, in its normal, build or
/// target-specific dependencies, is refused until this list says why it may.
const LIBRARY_DEPENDENCIES: &[(&str, &[&str])] = &[
    (
        "developer-tools/onbf-pack",
        &[
            "onbf-format",
            "onbf-merkle",
            "onbf-sign",
            "onbf-verify",
            "origin-crypto",
            "origin-version",
        ],
    ),
    ("developer-tools/onbf-sign", &["origin-crypto"]),
    ("developer-tools/oidl", &["origin-crypto"]),
];

/// The crates every command line may name besides its own library.
const COMMAND_LINE_CRATES: &[&str] = &["std", "core", "alloc"];

/// The modules of `std` that reach the host.
/// `std::path` among them: `Path::exists`, `is_file`, `metadata`, `read_dir` and
/// `canonicalize` read the file system (the review of 2026-11-14, finding F4).
const HOST_MODULES: &[&str] = &["fs", "process", "env", "net", "os", "time", "path"];

/// The standard streams, in `std::io`: refused as identifiers wherever they stand in a library,
/// so `use std::io::{self, Write}` and then `io::stderr()`, or `use std::io as sio` and then
/// `sio::stdout()`, are caught (the review of 2026-11-14, finding F3).
const STREAMS: &[&str] = &["stdin", "stdout", "stderr"];

/// The macros that print.
const PRINTS: &[&str] = &["print", "println", "eprint", "eprintln", "dbg"];

/// The value a string-literal token stands for, decoded.
fn literal(token: &Token<'_>) -> Option<String> {
    match token.kind {
        Kind::Str { raw: true } => Some(token.text.to_string()),
        Kind::Str { raw: false } => lex::unescape(token.text),
        _ => None,
    }
}

/// Every `const NAME: … = "…";` in `sources`: each name with its value.
pub(crate) fn string_consts(sources: &[&str]) -> BTreeMap<String, String> {
    let mut consts = BTreeMap::new();
    for source in sources {
        let tokens = lex::tokens(source);
        for (i, token) in tokens.iter().enumerate() {
            if !token.is_ident("const") {
                continue;
            }
            let (Some(name), Some(colon)) = (tokens.get(i + 1), tokens.get(i + 2)) else {
                continue;
            };
            if name.kind != Kind::Ident || !colon.is_punct(':') {
                continue;
            }
            // The value is the one literal between `=` and `;`.
            let Some(equals) = tokens
                .iter()
                .skip(i + 3)
                .position(|t| t.is_punct('=') || t.is_punct(';'))
                .map(|k| i + 3 + k)
            else {
                continue;
            };
            let is_equals = tokens.get(equals).is_some_and(|t| t.is_punct('='));
            if let (true, Some(value), Some(end)) =
                (is_equals, tokens.get(equals + 1), tokens.get(equals + 2))
                && end.is_punct(';')
                && let Some(text) = literal(value)
            {
                consts.insert(name.text.to_string(), text);
            }
        }
    }
    consts
}

/// What `token` stands for as text: a literal's value, or the value of a `const` it names.
fn text_of(token: &Token<'_>, consts: &BTreeMap<String, String>) -> Option<String> {
    literal(token).or_else(|| {
        (token.kind == Kind::Ident)
            .then(|| consts.get(token.text).cloned())
            .flatten()
    })
}

/// Whether `text`'s first word is Cargo's `run` or its built-in alias.
fn starts_run(text: &str) -> bool {
    text.split_whitespace()
        .next()
        .is_some_and(|word| RUN_WORDS.contains(&word))
}

/// The package an alias's expansion runs, if it names one.
fn package_of(words: &[String]) -> Option<String> {
    let mut words = words.iter();
    while let Some(word) = words.next() {
        if word == "--package" || word == "-p" {
            return words.next().cloned();
        }
        if let Some(name) = word.strip_prefix("--package=") {
            return Some(name.to_string());
        }
    }
    None
}

/// Each alias of `aliases` (name to expansion) that makes Cargo run a program: its expansion
/// begins with `run` or `r`, or with another alias that does (followed to a depth of 16). Each
/// maps to the package its expansion names, if it names one.
pub(crate) fn run_aliases(
    aliases: &BTreeMap<String, Vec<String>>,
) -> BTreeMap<String, Option<String>> {
    let mut runs = BTreeMap::new();
    for name in aliases.keys() {
        let mut words: Vec<String> = aliases.get(name).cloned().unwrap_or_default();
        for _ in 0..16 {
            let Some(first) = words.first().cloned() else {
                break;
            };
            if RUN_WORDS.contains(&first.as_str()) {
                runs.insert(name.clone(), package_of(&words));
                break;
            }
            match aliases.get(&first) {
                Some(expansion) if &first != name => {
                    words.splice(0..1, expansion.iter().cloned());
                }
                _ => break,
            }
        }
    }
    runs
}

/// The aliases in one Cargo configuration file: `[alias]`'s keys and top-level `alias.<name>`
/// keys, each with its expansion as a string (split on white space) or an array of strings.
pub(crate) fn aliases_in(text: &str) -> BTreeMap<String, Vec<String>> {
    let mut aliases = BTreeMap::new();
    let mut table = String::new();
    let mut lines = text.lines();
    while let Some(line) = lines.next() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') {
            table = line.trim_matches(['[', ']']).trim().to_string();
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim().trim_matches('"');
        let name = match (table.as_str(), key.strip_prefix("alias.")) {
            ("alias", _) => key,
            ("", Some(name)) => name,
            _ => continue,
        };
        let mut value = value.trim().to_string();
        // An array may go on over several lines, to its closing bracket.
        if value.starts_with('[') {
            while !value.contains(']') {
                let Some(more) = lines.next() else { break };
                value.push(' ');
                value.push_str(more.trim());
            }
        }
        let quoted: Vec<String> = value
            .split('"')
            .skip(1)
            .step_by(2)
            .map(str::to_string)
            .collect();
        let words = if value.starts_with('[') {
            quoted
        } else {
            quoted
                .first()
                .map(|text| text.split_whitespace().map(str::to_string).collect())
                .unwrap_or_default()
        };
        aliases.insert(name.trim_matches('"').to_string(), words);
    }
    aliases
}

/// The Cargo configuration files of the repository's root that hold its aliases:
/// `.cargo/config.toml` and `.cargo/config`, the files Cargo reads there when `xtask` runs it from
/// the root. Nothing else under the root is read: the main checkout holds other sessions'
/// worktrees, each with a `.cargo` of its own (the review's addendum, finding N3).
const ALIAS_FILES: &[&str] = &[".cargo/config.toml", ".cargo/config"];

/// The aliases of the repository root's Cargo configuration ([`ALIAS_FILES`]), each that exists.
pub(crate) fn repository_aliases(root: &Path) -> Result<BTreeMap<String, Vec<String>>> {
    let mut aliases = BTreeMap::new();
    for file in ALIAS_FILES {
        let path = root.join(file);
        if !path.is_file() {
            continue;
        }
        let text = String::from_utf8(read_file(&path)?)
            .map_err(|_| format!("{}: not UTF-8", path.display()))?;
        aliases.extend(aliases_in(&text));
    }
    Ok(aliases)
}

/// What G1 knows of the tree at `root`.
fn known(root: &Path) -> Result<Known> {
    let mut runs: BTreeMap<String, Option<String>> =
        RUN_WORDS.iter().map(|w| ((*w).to_string(), None)).collect();
    runs.extend(run_aliases(&repository_aliases(root)?));
    Ok(Known {
        programs: first_party_programs(root)?,
        runs,
    })
}

/// The first-party program `text` names, alone or as a path's last component.
fn names_program<'p>(text: &str, programs: &'p BTreeSet<String>) -> Option<&'p str> {
    let last = text.rsplit(['/', '\\']).next().unwrap_or(text);
    programs
        .get(text)
        .or_else(|| programs.get(last))
        .map(String::as_str)
}

/// The elements of an argument list, given as the tokens `.arg(…)` or `.args(…)` holds: the ranges
/// of `body`'s tokens between the commas of its list (the first `[…]` outside every other bracket,
/// as in `[…]`, `&[…]` or `vec![…]`), or the whole of `body` when it holds no list. A token outside
/// every element (after the list, as in `[…].iter()`) is in none.
fn elements(body: &[Token<'_>]) -> Vec<(usize, usize)> {
    let mut depth = 0;
    let mut open = None;
    for (k, token) in body.iter().enumerate() {
        if depth == 0 && token.is_punct('[') {
            open = Some(k);
            break;
        }
        depth += lex::bracket(token).unwrap_or(0);
    }
    let Some(open) = open else {
        return vec![(0, body.len())];
    };
    let close = lex::closing(body, open).unwrap_or(body.len());
    let mut found = Vec::new();
    let mut start = open + 1;
    let mut depth = 0;
    for (k, token) in body.iter().enumerate().take(close).skip(open + 1) {
        if depth == 0 && token.is_punct(',') {
            found.push((start, k));
            start = k + 1;
            continue;
        }
        depth += lex::bracket(token).unwrap_or(0);
    }
    if start < close {
        found.push((start, close));
    }
    found
}

/// The tokens inside the parentheses that open at `open`, if they do.
fn parenthesised<'t, 'a>(tokens: &'t [Token<'a>], open: usize) -> Option<&'t [Token<'a>]> {
    if !tokens.get(open)?.is_punct('(') {
        return None;
    }
    let close = lex::closing(tokens, open)?;
    tokens.get(open + 1..close)
}

/// Each function in `tokens`: its name and the range of token indices of its body.
fn functions<'a>(tokens: &[Token<'a>]) -> Vec<(&'a str, usize, usize)> {
    let mut found = Vec::new();
    for (i, token) in tokens.iter().enumerate() {
        if !token.is_ident("fn") {
            continue;
        }
        let Some(name) = tokens.get(i + 1).filter(|t| t.kind == Kind::Ident) else {
            continue;
        };
        // The body is the first `{` outside the signature's own brackets; a `;` first is a
        // declaration with no body.
        let mut depth = 0;
        for (k, t) in tokens.iter().enumerate().skip(i + 2) {
            if depth == 0 && t.is_punct(';') {
                break;
            }
            if depth == 0 && t.is_punct('{') {
                if let Some(close) = lex::closing(tokens, k) {
                    found.push((name.text, k, close));
                }
                break;
            }
            depth += lex::bracket(t).unwrap_or(0);
        }
    }
    found
}

/// The innermost function whose body holds token `at`.
fn enclosing<'a>(spans: &[(&'a str, usize, usize)], at: usize) -> Option<&'a str> {
    spans
        .iter()
        .filter(|(_, open, close)| *open < at && at < *close)
        .min_by_key(|(_, open, close)| close - open)
        .map(|(name, _, _)| *name)
}

/// G1's refusals in one `xtask` source file, `file` (its path under `developer-tools/xtask/`).
/// `consts` are the string constants of every `xtask` file; `programs` the first-party
/// programs' names.
pub(crate) fn program_hits(
    file: &str,
    source: &str,
    consts: &BTreeMap<String, String>,
    known: &Known,
) -> Vec<Hit> {
    let programs = &known.programs;
    let tokens = lex::tokens(source);
    let spans = functions(&tokens);
    let mut hits = Vec::new();
    let mut hit = |token: &Token<'_>, what: String| {
        hits.push(Hit {
            line: lex::line_of(source, token.start),
            what,
        });
    };
    for (i, token) in tokens.iter().enumerate() {
        // `.arg(…)` and `.args(…)`.
        let argument_list = token.is_punct('.')
            && tokens
                .get(i + 1)
                .is_some_and(|t| t.is_ident("arg") || t.is_ident("args"));
        if argument_list && let Some(body) = parenthesised(&tokens, i + 2) {
            let elements = elements(body);
            for (k, t) in body.iter().enumerate() {
                let Some(text) = text_of(t, consts) else {
                    continue;
                };
                // An alias's name is an option's value only when it is a whole element of the
                // list and the element before it is a literal option that takes a value; an
                // element that is not a literal is unknown and exempts nothing (the review's
                // addendum, finding N1).
                let after_value_option = elements
                    .iter()
                    .position(|&(start, end)| start == k && end == k + 1)
                    .and_then(|e| e.checked_sub(1))
                    .and_then(|e| elements.get(e))
                    .filter(|&&(start, end)| end == start + 1)
                    .and_then(|&(start, _)| body.get(start))
                    .and_then(|previous| text_of(previous, consts))
                    .is_some_and(|previous| VALUE_OPTIONS.contains(&previous.as_str()));
                // An alias defined in the argument list itself, as `--config`'s value (finding
                // N2).
                if text.starts_with("alias.") || text.starts_with("--config=alias.") {
                    hit(
                        t,
                        format!(
                            "`{text}` given to a command: an alias defined there can be `cargo run` of a first-party program (ADR-0011 Amendment 4, T1)"
                        ),
                    );
                    continue;
                }
                if starts_run(&text) {
                    hit(
                        t,
                        format!(
                            "`{text}` given to a command: `cargo run` starts a first-party program; call its library (ADR-0011 Amendment 4, T1)"
                        ),
                    );
                    continue;
                }
                // An alias of the repository's Cargo configuration that is `cargo run` under
                // another name (the review of 2026-11-14, finding F2), unless it is an option's
                // value, or `xtask` re-running itself where `SELF_RUNS` allows that.
                let first = text.split_whitespace().next().unwrap_or_default();
                let Some(runs) = known.runs.get(first).filter(|_| !after_value_option) else {
                    continue;
                };
                let place = enclosing(&spans, i);
                let self_run =
                    runs.as_deref() == Some("xtask") && place.is_some_and(|f| self_run(file, f));
                if !self_run {
                    hit(
                        t,
                        format!(
                            "`{text}` given to a command: Cargo's alias `{first}` is `cargo run`{}; call its library (ADR-0011 Amendment 4, T1)",
                            runs.as_deref()
                                .map_or(String::new(), |p| format!(" of `{p}`"))
                        ),
                    );
                }
            }
        }
        // An alias defined for one command, in its environment (finding F2).
        if let Some(text) = literal(token)
            && text.starts_with(ALIAS_VARIABLE)
        {
            hit(
                token,
                format!(
                    "`{text}`: an alias defined in a command's environment can be `cargo run` of a first-party program (ADR-0011 Amendment 4, T1)"
                ),
            );
        }
        // `Command::new(…)`.
        let new_command = token.is_ident("Command")
            && tokens.get(i + 1).is_some_and(|t| t.is_punct(':'))
            && tokens.get(i + 2).is_some_and(|t| t.is_punct(':'))
            && tokens.get(i + 3).is_some_and(|t| t.is_ident("new"));
        if new_command && let Some(body) = parenthesised(&tokens, i + 4) {
            for t in body {
                if let Some(text) = text_of(t, consts)
                    && let Some(program) = names_program(&text, programs)
                {
                    hit(
                        t,
                        format!(
                            "`Command::new` names `{program}`, a first-party program; call its library (ADR-0011 Amendment 4, T1)"
                        ),
                    );
                }
            }
        }
        // `xtask` naming itself.
        if token.is_ident("current_exe") {
            let place = enclosing(&spans, i);
            let listed = place.is_some_and(|f| self_run(file, f));
            if !listed {
                hit(
                    token,
                    format!(
                        "`current_exe` in {}: `xtask` re-runs itself only in the functions host_tools::SELF_RUNS lists, by decision items 300 and 310",
                        place.map_or("no function".to_string(), |f| format!("`{f}`"))
                    ),
                );
            }
        }
    }
    hits
}

/// The listed places of `SELF_RUNS` in `file` that no longer name `current_exe`.
fn stale_self_runs(file: &str, source: &str) -> Vec<&'static str> {
    let tokens = lex::tokens(source);
    let spans = functions(&tokens);
    let naming: BTreeSet<&str> = tokens
        .iter()
        .enumerate()
        .filter(|(_, t)| t.is_ident("current_exe"))
        .filter_map(|(i, _)| enclosing(&spans, i))
        .collect();
    SELF_RUNS
        .iter()
        .filter(|(f, function, _)| *f == file && !naming.contains(function))
        .map(|(_, function, _)| *function)
        .collect()
}

/// The token indices of `tokens` inside an item under `#[cfg(test)]`, as ranges.
fn test_items(tokens: &[Token<'_>]) -> Vec<(usize, usize)> {
    let mut ranges = Vec::new();
    for i in 0..tokens.len() {
        let attribute = ["#", "[", "cfg", "(", "test", ")", "]"];
        let is_attribute = attribute
            .iter()
            .enumerate()
            .all(|(k, text)| tokens.get(i + k).is_some_and(|t| t.text == *text));
        if !is_attribute {
            continue;
        }
        // The item ends at its first `;` or `,` outside brackets, or with the block it opens: an
        // item, a struct's field, an enum's variant or a match arm. A closing bracket that takes
        // the depth below where the item started ends it too, before that bracket: the item was
        // the last of a list with no comma after it. Nothing past that point is excluded (the
        // review of 2026-11-14, finding F1: a test-only field once hid the function after it).
        let mut depth = 0;
        for (k, t) in tokens.iter().enumerate().skip(i + attribute.len()) {
            if depth == 0 && (t.is_punct(';') || t.is_punct(',')) {
                ranges.push((i, k));
                break;
            }
            if depth == 0 && t.is_punct('{') {
                if let Some(close) = lex::closing(tokens, k) {
                    ranges.push((i, close));
                }
                break;
            }
            depth += lex::bracket(t).unwrap_or(0);
            if depth < 0 {
                ranges.push((i, k.saturating_sub(1)));
                break;
            }
        }
    }
    ranges
}

/// The paths a `std` path starting at token `at` (the `std`) names, as its segments after
/// `std`: `std::fs::File::open` is `["fs", "File", "open"]`, and `std::{fs, io::{self, stdout}}`
/// gives `["fs"]`, `["io", "self"]` and `["io", "stdout"]`. A glob is the segment `*`. A bare
/// `std` names none.
fn std_paths<'a>(tokens: &[Token<'a>], at: usize) -> Vec<Vec<&'a str>> {
    let mut paths = Vec::new();
    if is_separator(tokens, at + 1) {
        tree(tokens, at + 3, Vec::new(), &mut paths);
    }
    paths
}

/// Whether `::` starts at `i`.
fn is_separator(tokens: &[Token<'_>], i: usize) -> bool {
    tokens.get(i).is_some_and(|t| t.is_punct(':'))
        && tokens.get(i + 1).is_some_and(|t| t.is_punct(':'))
}

/// The paths the use-tree or path starting at `i` names after `prefix`, into `paths`: segments
/// joined by `::`, a `{…}` group of such trees, a `*`, and `as name`, which ends a path.
fn tree<'a>(
    tokens: &[Token<'a>],
    mut i: usize,
    mut path: Vec<&'a str>,
    paths: &mut Vec<Vec<&'a str>>,
) {
    loop {
        let Some(token) = tokens.get(i) else {
            paths.push(path);
            return;
        };
        if token.is_punct('{') {
            let Some(close) = lex::closing(tokens, i) else {
                paths.push(path);
                return;
            };
            // Each element that starts the group or follows a comma at its top level.
            let mut depth = 0;
            let mut element = true;
            for (k, t) in tokens.iter().enumerate().take(close).skip(i + 1) {
                if depth == 0 && element && !t.is_punct(',') {
                    tree(tokens, k, path.clone(), paths);
                    element = false;
                }
                if depth == 0 && t.is_punct(',') {
                    element = true;
                }
                depth += lex::bracket(t).unwrap_or(0);
            }
            return;
        }
        let is_segment =
            (token.kind == Kind::Ident && !token.is_ident("as")) || token.is_punct('*');
        if !is_segment {
            paths.push(path);
            return;
        }
        path.push(token.text);
        if !is_separator(tokens, i + 1) {
            paths.push(path);
            return;
        }
        i += 3;
    }
}

/// G2's refusals in one library source file.
pub(crate) fn library_hits(source: &str) -> Vec<Hit> {
    let tokens = lex::tokens(source);
    let tests = test_items(&tokens);
    let in_test = |i: usize| tests.iter().any(|(start, end)| *start <= i && i <= *end);
    let mut hits = Vec::new();
    let mut hit = |token: &Token<'_>, what: String| {
        hits.push(Hit {
            line: lex::line_of(source, token.start),
            what,
        });
    };
    for (i, token) in tokens.iter().enumerate() {
        if in_test(i) {
            continue;
        }
        if token.is_ident("std") {
            for path in std_paths(&tokens, i) {
                let shown = format!("std::{}", path.join("::"));
                let refused = match path.as_slice() {
                    [module, ..] if HOST_MODULES.contains(module) => {
                        Some(format!("`{shown}`: `std::{module}` reaches the host"))
                    }
                    _ if path.contains(&"*") => Some(format!(
                        "`{shown}`: a glob import from `std` can bring the host in unnamed"
                    )),
                    _ => None,
                };
                if let Some(why) = refused {
                    hit(
                        token,
                        format!("{why}; take it from the caller (ADR-0011 Amendment 4, T1)"),
                    );
                }
            }
        }
        if let Some(text) = literal(token)
            && text.starts_with("/dev/")
        {
            hit(
                token,
                format!(
                    "`{text}`: a device of the host; take its bytes from the caller (ADR-0011 Amendment 4, T1)"
                ),
            );
        }
        if token.kind == Kind::Ident && STREAMS.contains(&token.text) {
            hit(
                token,
                format!(
                    "`{}`: the standard streams reach the host; a library returns what it has to say (ADR-0011 Amendment 4, T1)",
                    token.text
                ),
            );
        }
        if token.kind == Kind::Ident
            && PRINTS.contains(&token.text)
            && tokens.get(i + 1).is_some_and(|t| t.is_punct('!'))
        {
            hit(
                token,
                format!(
                    "`{}!` prints: a library returns what it has to say (ADR-0011 Amendment 4, T1)",
                    token.text
                ),
            );
        }
    }
    hits
}

/// What kind of table `table` is, for [`library_dependencies`]: `Some(None)` for a table of
/// dependencies that reach the library's code (`dependencies`, `build-dependencies`, and either
/// under `target.<key>.`), `Some(Some(name))` for a table of one such dependency
/// (`[dependencies.<name>]`), and `None` for anything else, development dependencies included.
fn dependency_table(table: &str) -> Option<Option<String>> {
    if table.contains("dev-dependencies") {
        return None;
    }
    let at = table.find("dependencies")?;
    let before = table.get(..at)?;
    let reaches = before.is_empty()
        || before == "build-"
        || (before.starts_with("target.")
            && (before.ends_with('.') || before.ends_with(".build-")));
    if !reaches {
        return None;
    }
    let after = table.get(at + "dependencies".len()..)?;
    if after.is_empty() {
        return Some(None);
    }
    after
        .strip_prefix('.')
        .map(|name| Some(name.trim_matches('"').to_string()))
}

/// The dependencies a manifest names in the tables that reach its library's code:
/// `[dependencies]` and `[build-dependencies]`, their `target.<key>.` forms, `[dependencies.<name>]`
/// tables, and top-level `dependencies.<name>` keys. Development dependencies, which only its
/// tests see, are not among them.
pub(crate) fn library_dependencies(text: &str) -> BTreeSet<String> {
    let name = |key: &str| {
        key.split('.')
            .next()
            .unwrap_or(key)
            .trim()
            .trim_matches('"')
            .to_string()
    };
    let mut names = BTreeSet::new();
    let mut table = String::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') {
            table = line.trim_matches(['[', ']']).trim().to_string();
            if let Some(Some(dependency)) = dependency_table(&table) {
                names.insert(dependency);
            }
            continue;
        }
        let Some((key, _)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        if table.is_empty() {
            if let Some(Some(dependency)) = dependency_table(key) {
                names.insert(name(&dependency));
            }
        } else if dependency_table(&table) == Some(None) {
            names.insert(name(key));
        }
    }
    names
}

/// G3's refusals in a command line's `main.rs` (the review of 2026-11-14, finding F5): an
/// identifier that names a first-party crate other than its own library `own`, from `crates`,
/// and an `extern crate` of anything but `std`, `core`, `alloc` and `own`. So the command line
/// reaches no code but its library's and the standard library's. That it holds no logic of its
/// own is review's to see, not this gate's.
pub(crate) fn command_line_hits(source: &str, own: &str, crates: &BTreeSet<String>) -> Vec<Hit> {
    let tokens = lex::tokens(source);
    let allowed = |name: &str| COMMAND_LINE_CRATES.contains(&name) || name == own;
    let mut hits = Vec::new();
    for (i, token) in tokens.iter().enumerate() {
        let named =
            if token.kind == Kind::Ident && crates.contains(token.text) && !allowed(token.text) {
                Some(token.text)
            } else if token.is_ident("crate")
                && i > 0
                && tokens.get(i - 1).is_some_and(|t| t.is_ident("extern"))
            {
                tokens
                    .get(i + 1)
                    // A first-party crate is the identifier rule's, on its own token.
                    .filter(|t| {
                        matches!(t.kind, Kind::Ident | Kind::RawIdent)
                            && !allowed(t.text)
                            && !crates.contains(t.text)
                    })
                    .map(|t| t.text)
            } else {
                None
            };
        if let Some(name) = named {
            hits.push(Hit {
                line: lex::line_of(source, token.start),
                what: format!(
                    "`{name}`: a command line names no crate but `std`, `core`, `alloc` and its own library `{own}`; what it needs of another, its library offers (ADR-0011 Amendment 4, T1)"
                ),
            });
        }
    }
    hits
}

/// Whether `relative`, a path under a library's directory, is library source G2 reads.
fn is_library_source(relative: &Path) -> bool {
    let Ok(rest) = relative.strip_prefix("src") else {
        return false;
    };
    relative.extension().is_some_and(|e| e == "rs")
        && rest != Path::new("main.rs")
        && rest != Path::new("tests.rs")
        && !rest.starts_with("tests")
}

/// Every first-party program's name: each workspace package's, and each `[[bin]]` target's.
fn first_party_programs(root: &Path) -> Result<BTreeSet<String>> {
    let mut manifests = Vec::new();
    collect_files(
        root,
        &|_| false,
        &|name| name == "Cargo.toml",
        &mut manifests,
    )
    .map_err(|e| format!("host-tools gate: {e}"))?;
    let mut names = BTreeSet::new();
    for manifest in &manifests {
        let text = String::from_utf8(read_file(manifest)?)
            .map_err(|_| format!("{}: not UTF-8", manifest.display()))?;
        let mut table = String::new();
        for line in text.lines() {
            let line = line.trim();
            if line.starts_with('[') {
                table = line.trim_matches(['[', ']']).trim().to_string();
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            if key.trim() == "name" && (table == "package" || table == "bin") {
                let value = value.trim();
                if let Some(name) = value.strip_prefix('"').and_then(|v| v.strip_suffix('"')) {
                    names.insert(name.to_string());
                }
            }
        }
    }
    if names.is_empty() {
        return Err("host-tools gate: no package found in the tree's manifests".into());
    }
    Ok(names)
}

/// Each tool's command line, if it has one: its directory, its library's crate name, its
/// `src/main.rs`, and the crates G3 refuses it (every first-party crate's name, and its own
/// dependencies', as identifiers, but its own library's).
pub(crate) struct CommandLine {
    pub(crate) directory: &'static str,
    pub(crate) own: String,
    pub(crate) main: String,
    pub(crate) crates: BTreeSet<String>,
}

/// The command lines of the three tools that have one.
fn command_lines(root: &Path, known: &Known) -> Result<Vec<CommandLine>> {
    let mut lines = Vec::new();
    for library in LIBRARIES {
        let main = root.join(library).join("src").join("main.rs");
        if !main.is_file() {
            continue;
        }
        let manifest = String::from_utf8(read_file(&root.join(library).join("Cargo.toml"))?)
            .map_err(|_| format!("{library}/Cargo.toml: not UTF-8"))?;
        let own = library
            .rsplit('/')
            .next()
            .unwrap_or(library)
            .replace('-', "_");
        let crates = known
            .programs
            .iter()
            .chain(&library_dependencies(&manifest))
            .map(|name| name.replace('-', "_"))
            .collect();
        lines.push(CommandLine {
            directory: library,
            own,
            main: String::from_utf8(read_file(&main)?)
                .map_err(|_| format!("{}: not UTF-8", main.display()))?,
            crates,
        });
    }
    Ok(lines)
}

/// The dependencies each library's manifest names beyond [`LIBRARY_DEPENDENCIES`]'s.
fn unknown_dependencies(root: &Path) -> Result<Vec<String>> {
    let mut problems = Vec::new();
    for library in LIBRARIES {
        let manifest = String::from_utf8(read_file(&root.join(library).join("Cargo.toml"))?)
            .map_err(|_| format!("{library}/Cargo.toml: not UTF-8"))?;
        problems.extend(extra_dependencies(library, &manifest));
    }
    Ok(problems)
}

/// The dependencies `manifest`, `library`'s, names that [`LIBRARY_DEPENDENCIES`] does not give
/// it, each as a refusal.
pub(crate) fn extra_dependencies(library: &str, manifest: &str) -> Vec<String> {
    let allowed: &[&str] = LIBRARY_DEPENDENCIES
        .iter()
        .find(|(dir, _)| *dir == library)
        .map_or(&[], |(_, deps)| deps);
    library_dependencies(manifest)
        .into_iter()
        .filter(|dependency| !allowed.contains(&dependency.as_str()))
        .map(|dependency| {
            format!(
                "{library}/Cargo.toml: the library depends on `{dependency}`, which host_tools::LIBRARY_DEPENDENCIES does not list: G2 reads only the library's own sources (the review of 2026-11-14, finding F11)"
            )
        })
        .collect()
}

/// `xtask`'s source files: each path under `developer-tools/xtask/`, and its text.
fn xtask_sources(root: &Path) -> Result<Vec<(String, String)>> {
    let dir = root.join("developer-tools").join("xtask");
    let mut files = Vec::new();
    collect_files(&dir, &|_| false, &|name| name.ends_with(".rs"), &mut files)
        .map_err(|e| format!("host-tools gate: {e}"))?;
    let mut sources = Vec::new();
    for path in files {
        let relative = path
            .strip_prefix(&dir)
            .map_err(|e| format!("{}: {e}", path.display()))?
            .to_string_lossy()
            .replace('\\', "/");
        let text = String::from_utf8(read_file(&path)?)
            .map_err(|_| format!("{}: not UTF-8", path.display()))?;
        sources.push((relative, text));
    }
    Ok(sources)
}

/// The three libraries' sources G2 reads: each path from the repository's root, and its text.
fn library_sources(root: &Path) -> Result<Vec<(String, String)>> {
    let mut sources = Vec::new();
    for library in LIBRARIES {
        let dir = root.join(library);
        let mut files = Vec::new();
        collect_files(
            &dir.join("src"),
            &|_| false,
            &|name| name.ends_with(".rs"),
            &mut files,
        )
        .map_err(|e| format!("host-tools gate: {library}: {e}"))?;
        for path in files {
            let relative: PathBuf = path
                .strip_prefix(&dir)
                .map_err(|e| format!("{}: {e}", path.display()))?
                .to_path_buf();
            if !is_library_source(&relative) {
                continue;
            }
            let text = String::from_utf8(read_file(&path)?)
                .map_err(|_| format!("{}: not UTF-8", path.display()))?;
            sources.push((format!("{library}/{}", relative.display()), text));
        }
    }
    Ok(sources)
}

/// What `xtask` ran before T1 to convert the kernel: planted by the controls.
const PLANTED_CARGO_RUN: &str = r#"
fn planted_control_pack(input: &str, output: &str) -> Result {
    run_step(cargo().args([
        "run",
        "--quiet",
        "--release",
        "--package",
        PACK_PACKAGE,
        "--",
        "krn",
        "--input",
        input,
        "--output",
        output,
    ]))
}
"#;

/// A first-party program started from its path: planted by the controls.
const PLANTED_PROGRAM_PATH: &str = r#"
fn planted_control_oidl() -> Command {
    Command::new(target_dir().join("release").join("oidl"))
}
"#;

/// `xtask` naming itself outside the list: planted by the controls.
const PLANTED_SELF_RUN: &str = r#"
fn planted_control_again() -> Command {
    Command::new(std::env::current_exe().unwrap())
}
"#;

/// `xtask` run through Cargo's alias, from a function `SELF_RUNS` does not list (the review of
/// 2026-11-14, finding F2): planted by the controls.
const PLANTED_ALIAS_RUN: &str = r#"
fn planted_control_alias() -> Result {
    run_step(cargo().args(["xtask", "boot-test"]))
}
"#;

/// `xtask` run through Cargo's alias after an option whose value is not a literal (the review's
/// addendum, finding N1): planted by the controls.
const PLANTED_OPTION_VALUE: &str = r#"
fn planted_control_settings(settings: &str) -> Result {
    run_step(cargo().args(["--config", settings, "xtask", "boot-test"]))
}
"#;

/// What `onbf-pack`'s command line called before T1 (the review of 2026-11-14, finding F5):
/// planted by the controls.
const PLANTED_KEY_ID: &str = r#"
fn planted_control_key_id(keys: &onbf_pack::sign::PublicKeys) -> [u8; 32] {
    onbf_verify::key_id(&onbf_verify::PublicKey {
        ed25519: &keys.ed25519,
        mldsa: &keys.mldsa87,
    })
}
"#;

/// A dependency planted in `onbf-sign`'s manifest, in place of its `[dependencies]` line (the
/// review of 2026-11-14, finding F11).
const PLANTED_DEPENDENCY: &str = "[dependencies]\nonbf-pack = { path = \"../onbf-pack\" }\n";

/// What `onbf-sign` held before T1: planted by the controls.
const PLANTED_OS_RANDOM: &str = r#"
pub fn os_random(out: &mut [u8]) -> std::io::Result<()> {
    use std::io::Read;
    std::fs::File::open("/dev/urandom")?.read_exact(out)
}
"#;

/// A test-only field, and host access in the function after it (the review of 2026-11-14,
/// finding F1): planted by the controls.
const PLANTED_TEST_FIELD: &str = r#"
pub struct PlantedControlProbe {
    #[cfg(test)]
    pub probe: bool,
}
pub fn planted_control_read(path: &str) -> Vec<u8> {
    let bytes = std::fs::read(path).unwrap_or_default();
    bytes
}
"#;

/// A standard stream reached through `std::io` imported as a module (the review of 2026-11-14,
/// finding F3): planted by the controls.
const PLANTED_STREAM: &str = r#"
use std::io::{self, Write};
pub fn planted_control_say(text: &str) {
    let _ = writeln!(io::stderr(), "{text}");
}
"#;

/// The file system reached through `std::path` (the review of 2026-11-14, finding F4): planted by
/// the controls.
const PLANTED_PATH: &str = r#"
use std::path::Path;
pub fn planted_control_exists(path: &str) -> bool {
    Path::new(path).is_file()
}
"#;

/// The gate's negative controls, on copies of real files with a fault planted in each: each
/// must be refused. Returns how many were caught, or the controls that were not.
fn controls(
    main: &str,
    consts: &BTreeMap<String, String>,
    known: &Known,
    sign_lib: &str,
    pack_line: Option<&CommandLine>,
    sign_manifest: &str,
) -> std::result::Result<usize, Vec<String>> {
    let mut missed = Vec::new();
    for (planted, needle, name) in [
        (PLANTED_CARGO_RUN, "`run` given to a command", "cargo run"),
        (PLANTED_PROGRAM_PATH, "names `oidl`", "a program's path"),
        (
            PLANTED_SELF_RUN,
            "`current_exe` in `planted_control_again`",
            "a self-run",
        ),
        (
            PLANTED_ALIAS_RUN,
            "Cargo's alias `xtask` is `cargo run`",
            "`cargo xtask` outside SELF_RUNS",
        ),
        (
            PLANTED_OPTION_VALUE,
            "Cargo's alias `xtask` is `cargo run`",
            "`cargo xtask` after an option whose value is not a literal",
        ),
    ] {
        let text = format!("{main}\n{planted}");
        let hits = program_hits("src/main.rs", &text, consts, known);
        let real = program_hits("src/main.rs", main, consts, known).len();
        if hits.len() != real + 1 || !hits.iter().any(|h| h.what.contains(needle)) {
            missed.push(format!("{name} in developer-tools/xtask/src/main.rs"));
        }
    }
    let library_controls: &[(&str, &[&str], &str)] = &[
        (
            PLANTED_OS_RANDOM,
            &["`std::fs::File::open`", "`/dev/urandom`"],
            "os_random",
        ),
        (
            PLANTED_TEST_FIELD,
            &["`std::fs::read`"],
            "host access after a test-only field",
        ),
        (PLANTED_STREAM, &["`stderr`"], "a stream through `std::io`"),
        (PLANTED_PATH, &["`std::path::Path`"], "`std::path`"),
    ];
    let real = library_hits(sign_lib).len();
    for (planted, needles, name) in library_controls {
        let hits = library_hits(&format!("{sign_lib}\n{planted}"));
        let caught = needles
            .iter()
            .all(|needle| hits.iter().any(|h| h.what.contains(needle)));
        if !caught || hits.len() != real + needles.len() {
            missed.push(format!("{name} in developer-tools/onbf-sign/src/lib.rs"));
        }
    }
    // G3: the old command line's call of another crate (finding F5).
    match pack_line {
        Some(line) => {
            let real = command_line_hits(&line.main, &line.own, &line.crates).len();
            let planted = format!("{}\n{PLANTED_KEY_ID}", line.main);
            let hits = command_line_hits(&planted, &line.own, &line.crates);
            if hits.len() <= real || !hits.iter().any(|h| h.what.contains("`onbf_verify`")) {
                missed.push("onbf_verify::key_id in developer-tools/onbf-pack/src/main.rs".into());
            }
        }
        None => missed.push("developer-tools/onbf-pack/src/main.rs is not read".into()),
    }
    // The libraries' dependencies (finding F11).
    let planted = sign_manifest.replacen("[dependencies]\n", PLANTED_DEPENDENCY, 1);
    let extra = extra_dependencies("developer-tools/onbf-sign", &planted);
    if planted == sign_manifest || !extra.iter().any(|p| p.contains("`onbf-pack`")) {
        missed.push("a dependency in developer-tools/onbf-sign/Cargo.toml".into());
    }
    if missed.is_empty() {
        Ok(7 + library_controls.len())
    } else {
        Err(missed)
    }
}

/// The gate: see the module documentation.
pub(crate) fn gate() -> Result {
    let root = repo_root();
    let known = known(&root)?;
    let xtask = xtask_sources(&root)?;
    let texts: Vec<&str> = xtask.iter().map(|(_, text)| text.as_str()).collect();
    let consts = string_consts(&texts);
    let mut problems = Vec::new();
    for (file, text) in &xtask {
        for hit in program_hits(file, text, &consts, &known) {
            problems.push(format!(
                "developer-tools/xtask/{file}:{}: {}",
                hit.line, hit.what
            ));
        }
        for function in stale_self_runs(file, text) {
            problems.push(format!(
                "developer-tools/xtask/{file}: `{function}` is in host_tools::SELF_RUNS but no longer names `current_exe`: remove it from the list"
            ));
        }
    }
    for (file, _, _) in SELF_RUNS {
        if !xtask.iter().any(|(f, _)| f == file) {
            problems.push(format!(
                "host_tools::SELF_RUNS names developer-tools/xtask/{file}, which does not exist"
            ));
        }
    }
    let libraries = library_sources(&root)?;
    for (file, text) in &libraries {
        for hit in library_hits(text) {
            problems.push(format!("{file}:{}: {}", hit.line, hit.what));
        }
    }
    let lines = command_lines(&root, &known)?;
    for line in &lines {
        for hit in command_line_hits(&line.main, &line.own, &line.crates) {
            problems.push(format!(
                "{}/src/main.rs:{}: {}",
                line.directory, hit.line, hit.what
            ));
        }
    }
    problems.extend(unknown_dependencies(&root)?);
    let sign_manifest = String::from_utf8(read_file(
        &root.join("developer-tools/onbf-sign/Cargo.toml"),
    )?)
    .map_err(|_| "developer-tools/onbf-sign/Cargo.toml: not UTF-8".to_string())?;
    let main = xtask
        .iter()
        .find(|(file, _)| file == "src/main.rs")
        .map(|(_, text)| text.as_str())
        .ok_or("developer-tools/xtask/src/main.rs is missing")?;
    let sign_lib = libraries
        .iter()
        .find(|(file, _)| file == "developer-tools/onbf-sign/src/lib.rs")
        .map(|(_, text)| text.as_str())
        .ok_or("developer-tools/onbf-sign/src/lib.rs is missing")?;
    let pack_line = lines
        .iter()
        .find(|line| line.directory == "developer-tools/onbf-pack");
    let caught = match controls(main, &consts, &known, sign_lib, pack_line, &sign_manifest) {
        Ok(caught) => caught,
        Err(missed) => {
            problems.push(format!(
                "the gate's negative controls were not caught, so it would pass what it must refuse: {}",
                missed.join("; ")
            ));
            0
        }
    };
    if !problems.is_empty() {
        return Err(format!(
            "the host tools are libraries first (ADR-0011 Amendment 4, T1; its rules are in developer-tools/xtask/src/host_tools.rs):\n  {}",
            problems.join("\n  ")
        ));
    }
    println!(
        "xtask: host-tools gate: xtask ({} files) starts none of the {} first-party programs, by name or through `run` and the {} Cargo aliases that run one, and re-runs itself only in the {} places host_tools::SELF_RUNS lists (1 by decision item 300, the rest by item 310); the libraries of onbf-pack, onbf-sign and oidl ({} files) reach nothing of the host by the forms the gate reads, and depend on nothing but the crates it lists; their {} command lines name no crate but their own library and the standard library's; {caught} planted controls caught",
        xtask.len(),
        known.programs.len(),
        known.runs.len() - RUN_WORDS.len(),
        SELF_RUNS.len(),
        libraries.len(),
        lines.len()
    );
    Ok(())
}

#[cfg(test)]
mod tests;
