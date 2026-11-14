//! The host-tools gate (TASKS item 60): each refused form planted in the text of a real file is
//! caught, once, and named; the allowed forms pass; the real tree passes.

use super::*;

/// `xtask`'s source files, each with its text.
type Sources = Vec<(String, String)>;

/// The real `xtask` sources, their constants, and the first-party programs.
fn real() -> (Sources, BTreeMap<String, String>, BTreeSet<String>) {
    let root = repo_root();
    let sources = xtask_sources(&root).expect("xtask's sources");
    let texts: Vec<&str> = sources.iter().map(|(_, text)| text.as_str()).collect();
    let consts = string_consts(&texts);
    let programs = first_party_programs(&root).expect("the manifests");
    (sources, consts, programs)
}

fn text_of_file<'s>(sources: &'s [(String, String)], file: &str) -> &'s str {
    &sources
        .iter()
        .find(|(f, _)| f == file)
        .unwrap_or_else(|| panic!("{file}"))
        .1
}

/// G1's hits on `xtask`'s real `main.rs` with `planted` added at its end.
fn planted_in_main(planted: &str) -> Vec<Hit> {
    let (sources, mut consts, programs) = real();
    let main = text_of_file(&sources, "src/main.rs");
    let text = format!("{main}\n{planted}\n");
    // A `const` the planted text defines counts, as one in any xtask file would.
    consts.extend(string_consts(&[planted]));
    program_hits("src/main.rs", &text, &consts, &programs)
}

#[test]
fn the_real_tree_passes() {
    let (sources, consts, programs) = real();
    assert!(programs.contains("onbf-pack") && programs.contains("oidl"));
    assert!(programs.contains("xtask") && programs.contains("framekernel"));
    for (file, text) in &sources {
        assert_eq!(program_hits(file, text, &consts, &programs), [], "{file}");
        assert!(stale_self_runs(file, text).is_empty(), "{file}");
    }
    for (file, text) in library_sources(&repo_root()).unwrap() {
        assert_eq!(library_hits(&text), [], "{file}");
    }
}

#[test]
fn the_gate_s_own_controls_are_caught() {
    let (sources, consts, programs) = real();
    let sign_lib = library_sources(&repo_root())
        .unwrap()
        .into_iter()
        .find(|(file, _)| file == "developer-tools/onbf-sign/src/lib.rs")
        .unwrap()
        .1;
    assert_eq!(
        controls(
            text_of_file(&sources, "src/main.rs"),
            &consts,
            &programs,
            &sign_lib
        ),
        Ok(7)
    );
    // A control is missed when the rule it tests is gone: with no first-party programs, the
    // path control finds nothing to name.
    assert!(
        controls(
            text_of_file(&sources, "src/main.rs"),
            &consts,
            &BTreeSet::new(),
            &sign_lib
        )
        .is_err()
    );
}

#[test]
fn cargo_run_is_caught_however_it_is_spelled() {
    for planted in [
        // What `xtask` ran before T1, in its three shapes.
        r#"fn a() { run_step(cargo().args(["run", "--quiet", "--release", "--package", PACK_PACKAGE, "--", "krn"])); }"#,
        r#"fn b() { let mut c = cargo(); c.args(["run", "--quiet", "--package", OIDL_PACKAGE, "--"]); }"#,
        r#"fn c() { let mut c = cargo(); c.arg("run").arg("--package").arg("oidl"); }"#,
        // Cargo's alias, one string with its words, and a raw string.
        r#"fn d() { cargo().args(["r", "-p", "onbf-pack"]); }"#,
        r#"fn e() { cargo().args("run --package oidl --".split(' ')); }"#,
        r##"fn f() { cargo().arg(r#"run"#); }"##,
        // Through a constant.
        "const RUN_IT: &str = \"run\";\nfn g() { cargo().arg(RUN_IT).args([\"--package\", \"oidl\"]); }",
        // Inside a vector or a reference.
        r#"fn h() { cargo().args(&vec!["run", "-p", "oidl"]); }"#,
    ] {
        let hits = planted_in_main(planted);
        assert_eq!(hits.len(), 1, "{planted}: {hits:?}");
        assert!(
            hits[0].what.contains("given to a command") && hits[0].what.contains("T1"),
            "{hits:?}"
        );
    }
}

#[test]
fn what_is_not_cargo_run_passes() {
    for planted in [
        // xtask's own `run` command, its turn's label, and a test's symbol named `run`.
        r#"fn a(args: &[&str]) -> Result { match args { ["run"] => in_turn("run", run), _ => Ok(()) } }"#,
        r#"fn b() { let _ = ("run", &["ret"]); }"#,
        // Cargo's other subcommands on the host tools, and options that only begin with `r`.
        r#"fn c() { cargo().args(["test", "--package", PACK_PACKAGE]); cargo().args(["clippy", "--package", "oidl", "--release"]); }"#,
        r#"fn d() { cargo().args(["rustc", "--release"]).arg("-r"); }"#,
        // Text that says `cargo run` for a person to type.
        r#"fn e() -> String { format!("cargo run --package {OIDL_PACKAGE} -- fingerprint") }"#,
        // A person's own command, given to `exclusive`.
        r#"fn f(program: &str, args: &[String]) { Command::new(program).args(args); }"#,
    ] {
        assert_eq!(planted_in_main(planted), [], "{planted}");
    }
}

#[test]
fn a_first_party_program_by_name_or_path_is_caught() {
    for (planted, program) in [
        (r#"fn a() { Command::new("oidl"); }"#, "oidl"),
        (
            r#"fn b() { Command::new(target_dir().join("release").join("onbf-pack")); }"#,
            "onbf-pack",
        ),
        (
            r#"fn c() { Command::new("target/release/xtask"); }"#,
            "xtask",
        ),
        (r#"fn d() { Command::new(PACK_PACKAGE); }"#, "onbf-pack"),
        (
            r#"fn e() { std::process::Command::new("../target/debug/oidl"); }"#,
            "oidl",
        ),
        (r#"fn f() { Command::new("framekernel"); }"#, "framekernel"),
    ] {
        let hits = planted_in_main(planted);
        assert_eq!(hits.len(), 1, "{planted}: {hits:?}");
        assert!(
            hits[0].what.contains(&format!("names `{program}`")),
            "{hits:?}"
        );
    }
    // Outside programs pass.
    for planted in [
        r#"fn a() { Command::new("git"); Command::new("rustc"); Command::new(QEMU); }"#,
        r#"fn b() { Command::new(env::var_os("CARGO").unwrap_or_else(|| "cargo".into())); }"#,
        r#"fn c() { Command::new(toolchain.sysroot.join("bin").join("rustc")); }"#,
    ] {
        assert_eq!(planted_in_main(planted), [], "{planted}");
    }
}

#[test]
fn xtask_names_itself_only_where_the_list_allows() {
    // In a listed function of the listed file: allowed.
    let listed = "fn boot_matrix() -> Result { let exe = env::current_exe()?; Ok(()) }";
    let (_, consts, programs) = real();
    assert_eq!(program_hits("src/main.rs", listed, &consts, &programs), []);
    // The same function in another file, an unlisted function, and no function at all.
    let hits = program_hits("src/image.rs", listed, &consts, &programs);
    assert_eq!(hits.len(), 1, "{hits:?}");
    assert!(hits[0].what.contains("`boot_matrix`") && hits[0].what.contains("300"));
    let hits = planted_in_main("fn again() { Command::new(std::env::current_exe().unwrap()); }");
    assert_eq!(hits.len(), 1, "{hits:?}");
    assert!(hits[0].what.contains("`again`"), "{hits:?}");
    let hits = planted_in_main("static ME: fn() = || { let _ = std::env::current_exe(); };");
    assert_eq!(hits.len(), 1, "{hits:?}");
    assert!(hits[0].what.contains("no function"), "{hits:?}");
    // A closure inside a listed function is still inside it.
    let nested = "fn boot_x2apic() { let f = || std::env::current_exe(); }";
    assert_eq!(program_hits("src/main.rs", nested, &consts, &programs), []);
    // A listed place that no longer names itself is a stale allowance.
    assert_eq!(
        stale_self_runs("src/main.rs", "fn boot_matrix() {}"),
        SELF_RUNS
            .iter()
            .filter(|(file, _)| *file == "src/main.rs")
            .map(|(_, function)| *function)
            .collect::<Vec<_>>()
    );
}

#[test]
fn string_constants_are_read_with_their_values() {
    let consts = string_consts(&[
        "pub(crate) const A: &str = \"a\";\nconst B: &'static str = r#\"b\"#;\nconst C: usize = 3;\nconst D: &[&str] = &[\"d\"];\nconst E: &str = \"tab\\there\";",
    ]);
    assert_eq!(consts.get("A").map(String::as_str), Some("a"));
    assert_eq!(consts.get("B").map(String::as_str), Some("b"));
    assert_eq!(consts.get("E").map(String::as_str), Some("tab\there"));
    assert!(!consts.contains_key("C") && !consts.contains_key("D"));
}

/// G2's hits on `onbf-sign`'s real `lib.rs` with `planted` added at its end.
fn planted_in_library(planted: &str) -> Vec<Hit> {
    let sign_lib = library_sources(&repo_root())
        .unwrap()
        .into_iter()
        .find(|(file, _)| file == "developer-tools/onbf-sign/src/lib.rs")
        .unwrap()
        .1;
    library_hits(&format!("{sign_lib}\n{planted}\n"))
}

#[test]
fn host_access_in_a_library_is_caught_however_it_is_spelled() {
    for (planted, needle) in [
        // What the libraries held before T1.
        (
            "pub fn os_random(out: &mut [u8]) -> std::io::Result<()> { std::fs::File::open(\"x\")?; Ok(()) }",
            "std::fs::File::open",
        ),
        (
            "fn r() { let _ = std::process::Command::new(\"rustc\"); }",
            "std::process",
        ),
        ("fn a() { let _ = ::std::fs::read(\"x\"); }", "std::fs"),
        ("use std::{collections::BTreeMap, fs};", "std::fs"),
        (
            "use std::{collections::BTreeMap, env::var};",
            "std::env::var",
        ),
        ("use std::{io::{self, Write}, net};", "std::net"),
        ("use std::os::unix::fs::PermissionsExt;", "std::os"),
        (
            "fn t() { let _ = std::time::SystemTime::now(); }",
            "std::time",
        ),
        ("use std::fs as files;", "std::fs"),
        // The standard streams.
        ("use std::io::{self, stdout};", "`stdout`"),
        ("fn e() { let _ = std::io::stderr(); }", "`stderr`"),
        ("use std::io::stdin;", "`stdin`"),
        // Through `std::io` imported as a module or renamed (finding F3).
        (
            "use std::io::{self, Write};\nfn e() { let _ = writeln!(io::stderr(), \"x\"); }",
            "`stderr`",
        ),
        (
            "use std::io as sio;\nfn o() { let _ = sio::stdout(); }",
            "`stdout`",
        ),
        (
            "use std::io::stdin as input;\nfn i() { let _ = input(); }",
            "`stdin`",
        ),
        // `std::path` reaches the file system (finding F4).
        (
            "use std::path::Path;\nfn x(p: &str) -> bool { Path::new(p).is_file() }",
            "`std::path::Path`",
        ),
        (
            "fn y(p: &str) -> bool { ::std::path::Path::new(p).exists() }",
            "`std::path::Path::new`",
        ),
        (
            "use std::{collections::BTreeMap, path::PathBuf};",
            "`std::path::PathBuf`",
        ),
        // Globs.
        ("use std::*;", "std::*"),
        ("use std::io::*;", "std::io::*"),
        ("use std::{collections::*};", "std::collections::*"),
        // A device, and printing.
        ("const DEV: &str = \"/dev/urandom\";", "/dev/urandom"),
        ("fn p() { println!(\"x\"); }", "println!"),
        ("fn p() { eprintln!(\"x\"); }", "eprintln!"),
        ("fn p() { print!(\"x\"); }", "print!"),
        ("fn p() { eprint!(\"x\"); }", "eprint!"),
        ("fn p(x: u8) -> u8 { dbg!(x) }", "dbg!"),
    ] {
        let hits = planted_in_library(planted);
        assert!(
            !hits.is_empty() && hits.iter().any(|h| h.what.contains(needle)),
            "{planted}: {hits:?}"
        );
    }
    // `use std::fs::*` is both a host module and a glob: one path, refused once.
    assert_eq!(planted_in_library("use std::fs::*;").len(), 1);
}

#[test]
fn what_reaches_no_host_passes() {
    for planted in [
        "use std::io::{Read, Write};",
        "use std::collections::{BTreeMap, BTreeSet};",
        "fn f() -> std::io::Result<()> { Ok(()) }",
        "use core::time::Duration;",
        "use std::fmt::Write as _;",
        "use std::{rc::Rc, cell::RefCell};",
        "fn w(out: &mut String) { let _ = writeln!(out, \"x\"); let _ = format!(\"println!\"); }",
        "const NOT_A_DEVICE: &str = \"dev/null and /device\";",
        "fn g() { let _ = include_str!(\"template.rs.in\"); }",
        "// std::fs::read in a comment, and println!",
        "const TEXT: &str = \"use std::process::Command; println!()\";",
        "extern crate std;",
    ] {
        assert_eq!(planted_in_library(planted), [], "{planted}");
    }
}

#[test]
fn test_items_are_not_library_code() {
    for planted in [
        "#[cfg(test)]\nmod tests { use std::fs; fn f() { println!(\"x\"); let _ = std::env::var(\"X\"); } }",
        "#[cfg(test)]\nuse std::fs;",
        "#[cfg(test)]\n#[path = \"../x.rs\"]\nmod testing;",
        "#[cfg(test)]\nfn helper() { eprintln!(\"x\"); }",
    ] {
        assert_eq!(planted_in_library(planted), [], "{planted}");
    }
    // Only `cfg(test)` itself: a `not(test)` item is library code.
    let hits = planted_in_library("#[cfg(not(test))]\nfn f() { let _ = std::fs::read(\"x\"); }");
    assert_eq!(hits.len(), 1, "{hits:?}");
    // And the item after a test item is checked again.
    let hits =
        planted_in_library("#[cfg(test)]\nmod tests {}\nfn f() { let _ = std::fs::read(\"x\"); }");
    assert_eq!(hits.len(), 1, "{hits:?}");
}

#[test]
fn which_files_are_library_source() {
    for (path, library) in [
        ("src/lib.rs", true),
        ("src/codegen/rust.rs", true),
        ("src/main.rs", false),
        ("src/tests.rs", false),
        ("src/tests/sign.rs", false),
        ("src/tests/mod.rs", false),
        ("tests/rustc_kernel.rs", false),
        ("src/codegen/oidl_rt.rs.in", false),
    ] {
        assert_eq!(is_library_source(Path::new(path)), library, "{path}");
    }
}

/// A `#[cfg(test)]` on a field, a variant or a match arm ends at its comma, or before the bracket
/// that closes its list, and hides nothing after it (the review of 2026-11-14, finding F1).
#[test]
fn a_test_only_field_variant_or_arm_hides_nothing_after_it() {
    let after =
        "pub fn read_key(p: &str) -> Vec<u8> { let b = std::fs::read(p).unwrap_or_default(); b }";
    for (planted, what) in [
        ("pub struct S { #[cfg(test)] pub probe: bool, }", "a field"),
        (
            "pub struct S { pub a: u8, #[cfg(test)] pub probe: bool }",
            "a last field with no comma",
        ),
        ("pub enum E { A, #[cfg(test)] Probe, B }", "a variant"),
        (
            "pub enum E { A, #[cfg(test)] Probe }",
            "a last variant with no comma",
        ),
        (
            "pub fn m(x: u8) -> u8 { match x { #[cfg(test)] 9 => 1, _ => 0 } }",
            "a match arm",
        ),
        (
            "pub fn m(x: u8) -> u8 { match x { 1 => 2, #[cfg(test)] 9 => { 1 } _ => 0 } }",
            "a match arm with a block",
        ),
        (
            "pub fn f(#[cfg(test)] probe: u8, x: u8) -> u8 { x }",
            "a parameter",
        ),
    ] {
        let hits = planted_in_library(&format!("{planted}\n{after}"));
        assert_eq!(hits.len(), 1, "{what}: {hits:?}");
        assert!(hits[0].what.contains("std::fs::read"), "{what}: {hits:?}");
    }
    // The test-only code itself stays excluded: a test field whose type names `std::fs`.
    let hits =
        planted_in_library("pub struct S { #[cfg(test)] pub probe: std::fs::File, pub b: u8 }");
    assert_eq!(hits, [], "{hits:?}");
}
