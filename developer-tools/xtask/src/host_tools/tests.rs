//! The host-tools gate (TASKS item 60): each refused form planted in the text of a real file is
//! caught, once, and named; the allowed forms pass; the real tree passes.

use super::*;

/// `xtask`'s source files, each with its text.
type Sources = Vec<(String, String)>;

/// The real `xtask` sources, their constants, and the first-party programs.
fn real() -> (Sources, BTreeMap<String, String>, Known) {
    let root = repo_root();
    let sources = xtask_sources(&root).expect("xtask's sources");
    let texts: Vec<&str> = sources.iter().map(|(_, text)| text.as_str()).collect();
    let consts = string_consts(&texts);
    let programs = known(&root).expect("the manifests and the Cargo configuration");
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
    assert!(programs.programs.contains("onbf-pack") && programs.programs.contains("oidl"));
    assert!(programs.programs.contains("xtask") && programs.programs.contains("framekernel"));
    assert_eq!(programs.runs.get("xtask"), Some(&Some("xtask".to_string())));
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
    let root = repo_root();
    let pack_line = command_lines(&root, &programs)
        .unwrap()
        .into_iter()
        .find(|line| line.directory == "developer-tools/onbf-pack");
    let sign_manifest =
        String::from_utf8(read_file(&root.join("developer-tools/onbf-sign/Cargo.toml")).unwrap())
            .unwrap();
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
            &sign_lib,
            pack_line.as_ref(),
            &sign_manifest
        ),
        Ok(11)
    );
    // A control is missed when the rule it tests is gone: with no first-party programs, the
    // path control finds nothing to name.
    assert!(
        controls(
            text_of_file(&sources, "src/main.rs"),
            &consts,
            &Known::default(),
            &sign_lib,
            pack_line.as_ref(),
            &sign_manifest
        )
        .is_err()
    );
    // And with no command line read, G3's control is missed.
    assert!(
        controls(
            text_of_file(&sources, "src/main.rs"),
            &consts,
            &programs,
            &sign_lib,
            None,
            &sign_manifest
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
    // Each listed place names its authority: item 300 for `boot_matrix`, item 311 for the three
    // scenario commands TASKS item 58 added, item 310 for the rest (the review of 2026-11-14,
    // finding F6; all three accepted by the owner).
    for (file, function, why) in SELF_RUNS {
        let expected = match *function {
            "boot_matrix" => "decision item 300",
            "boot_sparse" | "boot_many" | "boot_listed" => "decision item 311",
            _ => "decision item 310",
        };
        assert!(why.contains(expected), "{file} {function}: {why}");
        assert!(!why.contains("pending"), "{file} {function}: {why}");
    }
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
            .filter(|(file, _, _)| *file == "src/main.rs")
            .map(|(_, function, _)| *function)
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

/// Cargo's aliases, read as Cargo reads them: `[alias]` keys and dotted `alias.<name>` keys, a
/// string split on white space or an array of strings, followed alias to alias.
#[test]
fn cargo_aliases_are_read_and_followed() {
    let text = "[alias]\nxtask = \"run --quiet --package xtask --\"\n# pack = \"run\"\nb = \"build\"\nbig = [\n  \"run\",\n  \"--release\", \"-p\", \"onbf-pack\",\n]\nagain = \"xtask boot-test\"\nloop1 = \"loop2\"\nloop2 = \"loop1\"\n\n[target.x]\nrustflags = [\"-C\", \"x\"]\n";
    let aliases = aliases_in(text);
    assert_eq!(
        aliases.get("xtask"),
        Some(&vec![
            "run".to_string(),
            "--quiet".into(),
            "--package".into(),
            "xtask".into(),
            "--".into()
        ])
    );
    assert_eq!(aliases.get("big").map(Vec::len), Some(4));
    assert!(!aliases.contains_key("pack") && !aliases.contains_key("rustflags"));
    let runs = run_aliases(&aliases);
    assert_eq!(runs.get("xtask"), Some(&Some("xtask".to_string())));
    assert_eq!(runs.get("big"), Some(&Some("onbf-pack".to_string())));
    assert_eq!(
        runs.get("again"),
        Some(&Some("xtask".to_string())),
        "an alias of an alias"
    );
    assert!(
        !runs.contains_key("b") && !runs.contains_key("loop1"),
        "{runs:?}"
    );
    let dotted = aliases_in("alias.pack = \"run --package=onbf-pack --\"\n");
    assert_eq!(
        run_aliases(&dotted).get("pack"),
        Some(&Some("onbf-pack".to_string()))
    );
}

/// `cargo xtask` is `cargo run` under another name (the review of 2026-11-14, finding F2):
/// refused outside `SELF_RUNS`, allowed in a listed function, and allowed as an option's value.
#[test]
fn cargo_s_aliases_are_cargo_run() {
    for (planted, refused) in [
        // The planted control, and its spellings.
        (
            r#"fn a() { run_step(cargo().args(["xtask", "boot-test"])); }"#,
            true,
        ),
        (r#"fn b() { cargo().arg("xtask").arg("check"); }"#, true),
        (
            r#"fn c() { cargo().args(["--quiet", "xtask", "check"]); }"#,
            true,
        ),
        (r#"fn d() { cargo().args(["xtask boot-test"]); }"#, true),
        (
            r#"fn e() { cargo().env("CARGO_ALIAS_PACK", "run --package onbf-pack --").arg("pack"); }"#,
            true,
        ),
        // `xtask` as a package's name, the value of an option.
        (
            r#"fn f() { cargo().args(["test", "--package", "xtask"]); }"#,
            false,
        ),
        (
            r#"fn g() { cargo().args(["clippy", "-p", "xtask", "--all-targets"]); }"#,
            false,
        ),
        // In a function `SELF_RUNS` lists: `xtask` re-running itself.
        (
            r#"fn boot_matrix() { cargo().args(["xtask", "boot-test"]); }"#,
            false,
        ),
        // The option before an alias takes its value only from the element before it, and an
        // element that is not a literal is unknown (the review's addendum, finding N1).
        (
            r#"fn h(settings: &str) { cargo().args(["--config", &settings, "xtask", "boot-test"]); }"#,
            true,
        ),
        (
            r#"fn i(color: &str) { cargo().args(["--color", &color, "xtask", "boot-test"]); }"#,
            true,
        ),
        (
            r#"fn j(name: &str) { cargo().args(["--package", name, "xtask"]); }"#,
            true,
        ),
        (
            r#"fn k() { cargo().args(vec!["test", "--package", "xtask"]); }"#,
            false,
        ),
        (
            r#"fn l() { cargo().args(["test", "--package", "xtask"].iter()); }"#,
            false,
        ),
        (
            r#"fn m() { cargo().args(["test", "--package", "xtask", "--", "-q"]); }"#,
            false,
        ),
        // An alias defined in the argument list itself (finding N2).
        (
            r#"fn n() { cargo().args(["--config", "alias.pack=\"run -p onbf-pack\"", "pack", "krn"]); }"#,
            true,
        ),
        (
            r#"fn o() { cargo().arg("--config=alias.pack=run").arg("pack"); }"#,
            true,
        ),
    ] {
        let hits = planted_in_main(planted);
        assert_eq!(hits.len(), usize::from(refused), "{planted}: {hits:?}");
    }
    // An alias that runs another package is refused even in a listed function.
    let (_, consts, mut known) = real();
    known
        .runs
        .insert("pack".into(), Some("onbf-pack".to_string()));
    let hits = program_hits(
        "src/main.rs",
        r#"fn boot_matrix() { cargo().args(["pack", "krn"]); }"#,
        &consts,
        &known,
    );
    assert_eq!(hits.len(), 1, "{hits:?}");
    assert!(
        hits[0].what.contains("`cargo run` of `onbf-pack`"),
        "{hits:?}"
    );
}

/// The dependencies that reach a library's code, read from its manifest as Cargo reads them;
/// development dependencies are not among them.
#[test]
fn a_library_s_dependencies_are_read_from_every_table_that_reaches_it() {
    let manifest = "[package]\nname = \"x\"\n\n[dependencies]\norigin-crypto.workspace = true\nonbf-format = { path = \"../f\" }\n\"quoted\" = \"1\"\n\n[build-dependencies]\nbuilder = \"1\"\n\n[target.'cfg(unix)'.dependencies]\nunix-only = \"1\"\n\n[target.x86_64-apple-darwin.build-dependencies]\nmac-build = \"1\"\n\n[dependencies.tabled]\npath = \"../t\"\n\n[dev-dependencies]\ntest-only = \"1\"\n\n[target.'cfg(unix)'.dev-dependencies]\nunix-test = \"1\"\n\n[lints]\nworkspace = true\n";
    let found: Vec<String> = library_dependencies(manifest).into_iter().collect();
    assert_eq!(
        found,
        [
            "builder",
            "mac-build",
            "onbf-format",
            "origin-crypto",
            "quoted",
            "tabled",
            "unix-only"
        ]
    );
    assert_eq!(
        library_dependencies("dependencies.top = { path = \"../t\" }\n[package]\nname = \"x\"\n"),
        BTreeSet::from(["top".to_string()])
    );
}

/// Each library's manifest names only the dependencies `LIBRARY_DEPENDENCIES` gives it (the
/// review of 2026-11-14, finding F11), and one more is refused.
#[test]
fn a_library_depends_only_on_the_crates_the_gate_lists() {
    let root = repo_root();
    for (library, _) in LIBRARY_DEPENDENCIES {
        let manifest =
            String::from_utf8(read_file(&root.join(library).join("Cargo.toml")).unwrap()).unwrap();
        assert_eq!(
            extra_dependencies(library, &manifest),
            Vec::<String>::new(),
            "{library}"
        );
        for planted in [
            "[dependencies]\nonbf-pack = { path = \"../onbf-pack\" }\n",
            "[build-dependencies]\nhelper = { path = \"../helper\" }\n",
            "[target.'cfg(unix)'.dependencies]\nhelper = \"1\"\n",
        ] {
            let extra = extra_dependencies(library, &format!("{manifest}\n{planted}"));
            assert_eq!(extra.len(), 1, "{library}: {planted}: {extra:?}");
        }
    }
}

/// A command line names no crate but `std`, `core`, `alloc` and its own library (the review of
/// 2026-11-14, finding F5).
#[test]
fn a_command_line_names_no_crate_but_its_library_and_std() {
    let (_, _, known) = real();
    let lines = command_lines(&repo_root(), &known).unwrap();
    let directories: Vec<&str> = lines.iter().map(|line| line.directory).collect();
    assert_eq!(
        directories,
        ["developer-tools/onbf-pack", "developer-tools/oidl"]
    );
    for line in &lines {
        assert_eq!(
            command_line_hits(&line.main, &line.own, &line.crates),
            [],
            "{}",
            line.directory
        );
        for (planted, crate_named) in [
            ("fn a() { let _ = onbf_verify::key_id; }", "onbf_verify"),
            ("use origin_crypto::Sha256;", "origin_crypto"),
            ("extern crate onbf_format;", "onbf_format"),
            (
                "extern crate somewhere_else as elsewhere;",
                "somewhere_else",
            ),
            ("fn b() { let _ = ::onbf_sign::wipe; }", "onbf_sign"),
            ("fn c() { let _ = xtask::run; }", "xtask"),
        ] {
            let hits = command_line_hits(
                &format!("{}\n{planted}", line.main),
                &line.own,
                &line.crates,
            );
            assert_eq!(hits.len(), 1, "{}: {planted}: {hits:?}", line.directory);
            assert!(
                hits[0].what.contains(&format!("`{crate_named}`")),
                "{hits:?}"
            );
        }
        for allowed in [
            "extern crate std;",
            "extern crate alloc;",
            "use core::fmt;",
            "fn d() { let _ = std::fs::read(\"x\"); }",
        ] {
            let text = format!("{}\n{allowed}", line.main);
            assert_eq!(
                command_line_hits(&text, &line.own, &line.crates),
                [],
                "{allowed}"
            );
        }
        let own = format!("{}\nfn e() {{ let _ = {}::VERSION; }}", line.main, line.own);
        assert_eq!(command_line_hits(&own, &line.own, &line.crates), []);
    }
}

/// The aliases come from the repository root's Cargo configuration and nothing else under it:
/// another checkout's `.cargo` (the main checkout holds other sessions' worktrees), a file that
/// is not UTF-8 there, or a non-configuration file in the root's `.cargo` changes nothing (the
/// review's addendum, finding N3).
#[test]
fn aliases_are_read_from_the_root_s_configuration_alone() {
    let root = crate::target_dir()
        .join("xtask")
        .join("gate-tests")
        .join("root-aliases");
    let _ = std::fs::remove_dir_all(&root);
    for dir in [".cargo", "worktree/.cargo", "deep/a/.cargo"] {
        std::fs::create_dir_all(root.join(dir)).unwrap();
    }
    std::fs::write(
        root.join(".cargo/config.toml"),
        "[alias]\nxtask = \"run --quiet --package xtask --\"\n",
    )
    .unwrap();
    std::fs::write(root.join(".cargo/.DS_Store"), [0xFF, 0xFE, 0x00, 0x80]).unwrap();
    std::fs::write(
        root.join("worktree/.cargo/config.toml"),
        "[alias]\nxtask = \"build\"\n",
    )
    .unwrap();
    std::fs::write(
        root.join("deep/a/.cargo/config.toml"),
        [0xFF, 0xFE, 0x00, 0x80],
    )
    .unwrap();
    let aliases = repository_aliases(&root).unwrap();
    assert_eq!(aliases.len(), 1, "{aliases:?}");
    assert_eq!(
        run_aliases(&aliases).get("xtask"),
        Some(&Some("xtask".to_string()))
    );
    // A root with no configuration has no aliases; one whose configuration is not UTF-8 fails.
    std::fs::remove_file(root.join(".cargo/config.toml")).unwrap();
    assert!(repository_aliases(&root).unwrap().is_empty());
    std::fs::write(root.join(".cargo/config"), [0xFF, 0xFE]).unwrap();
    assert!(repository_aliases(&root).is_err());
    let _ = std::fs::remove_dir_all(&root);
}

/// The test-only-item scan before the fix for finding F1: an item ended only at a depth-0 `;` or
/// with its block.
fn scan_before_f1(tokens: &[Token<'_>]) -> Vec<(usize, usize)> {
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
        let mut depth = 0;
        for (k, t) in tokens.iter().enumerate().skip(i + attribute.len()) {
            if depth == 0 && t.is_punct(';') {
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
        }
    }
    ranges
}

/// The per-run control for finding F1 is one the scan before the fix would miss: its `std::fs`
/// lies inside the range that scan excluded, and outside every range the fixed scan excludes (the
/// review's addendum, finding N4).
#[test]
fn the_test_field_control_is_missed_by_the_scan_before_the_fix() {
    let tokens = lex::tokens(PLANTED_TEST_FIELD);
    let fs = tokens
        .iter()
        .enumerate()
        .position(|(k, t)| {
            t.is_ident("fs") && k >= 3 && tokens.get(k - 3).is_some_and(|s| s.is_ident("std"))
        })
        .expect("the control names std::fs");
    let inside = |ranges: &[(usize, usize)]| ranges.iter().any(|&(a, b)| a <= fs && fs <= b);
    assert!(inside(&scan_before_f1(&tokens)), "the old scan hid it");
    assert!(!inside(&test_items(&tokens)), "the fixed scan does not");
    assert_eq!(library_hits(PLANTED_TEST_FIELD).len(), 1);
}
