//! Compiles and runs what the generator writes (docs/spec/OIDL.md §17): the generated Rust is
//! built with the pinned toolchain for every target, the C headers with the host's C and C++
//! compilers, the corpus is run against the generated decoders, and faults planted in the
//! generated code show that the corpus would have caught each of them.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, OnceLock};

use oidl::model::{Compilation, ConstKind};
use oidl::{Input, codegen, compile, corpus, fingerprint};

const KERNEL: &str = include_str!("../../../sdk/interfaces/origin.kernel.oidl");
const DIRECTORY: &str = include_str!("../../../sdk/interfaces/origin.directory.oidl");
const ECHO: &str = include_str!("fixtures/echo.oidl");
const CALLS: &str = include_str!("fixtures/calls.oidl");
const VERSIONS: &str = include_str!("../../../origin-version.toml");

fn inputs() -> Vec<Input> {
    [
        ("origin.kernel.oidl", KERNEL),
        ("origin.directory.oidl", DIRECTORY),
        ("echo.oidl", ECHO),
        ("calls.oidl", CALLS),
    ]
    .iter()
    .map(|(file, text)| Input {
        file: file.to_string(),
        text: text.as_bytes().to_vec(),
    })
    .collect()
}

fn compilation() -> Compilation {
    compile(&inputs(), Some(VERSIONS)).expect("the definitions are valid")
}

/// Every file the tool writes: generated code, fingerprints and corpora.
fn all_files(comp: &Compilation) -> BTreeMap<String, String> {
    let mut files = BTreeMap::new();
    for file in codegen::generate(comp) {
        files.insert(file.path, file.text);
    }
    for library in &comp.libraries {
        files.insert(
            format!("abi/{}.fingerprint", library.name),
            fingerprint::fingerprint_file(library),
        );
        files.insert(
            format!("corpus/{}.corpus", library.name),
            corpus::corpus_for(library),
        );
    }
    files
}

fn scratch(name: &str) -> PathBuf {
    let base = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("oidl-generated");
    let dir = base.join(format!("{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create the scratch directory");
    dir
}

fn write_tree(dir: &Path, files: &BTreeMap<String, String>) {
    for (path, text) in files {
        let path = dir.join(path);
        fs::create_dir_all(path.parent().expect("a parent")).expect("mkdir");
        fs::write(path, text).expect("write");
    }
}

fn sysroot() -> PathBuf {
    let out = Command::new("rustc")
        .args(["--print", "sysroot"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("rustc");
    PathBuf::from(String::from_utf8(out.stdout).expect("utf8").trim())
}

/// The pinned toolchain's compiler.
fn rustc() -> PathBuf {
    sysroot().join("bin/rustc")
}

fn run_ok(mut command: Command, what: &str) -> String {
    let out = command.output().unwrap_or_else(|e| panic!("{what}: {e}"));
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(out.status.success(), "{what} failed:\n{stdout}\n{stderr}");
    stdout
}

fn host_rlib(dir: &Path, extra: &[&str]) -> PathBuf {
    let rlib = dir.join("liboidl_generated.rlib");
    let mut cmd = Command::new(rustc());
    cmd.args([
        "--edition",
        "2024",
        "--crate-type",
        "rlib",
        "--crate-name",
        "oidl_generated",
    ])
    .arg("-o")
    .arg(&rlib)
    .arg(dir.join("rust/lib.rs"))
    .args(extra)
    .current_dir(dir);
    run_ok(cmd, "compile the generated crate");
    rlib
}

/// Compiles and runs a program against the generated crate; returns what it printed.
fn run_program(dir: &Path, rlib: &Path, source: &Path, name: &str, extra: &[&str]) -> String {
    let program = dir.join(name);
    let mut cmd = Command::new(rustc());
    cmd.args([
        "--edition",
        "2024",
        "--crate-type",
        "bin",
        "--crate-name",
        name,
    ])
    .arg("--extern")
    .arg(format!("oidl_generated={}", rlib.display()))
    .arg("-o")
    .arg(&program)
    .arg(source)
    .args(extra)
    .current_dir(dir);
    run_ok(cmd, &format!("compile {name}"));
    run_ok(Command::new(&program), &format!("run {name}"))
}

struct Tree {
    dir: PathBuf,
    rlib: PathBuf,
    files: BTreeMap<String, String>,
}

/// One generated tree, built once and shared by the tests that only read it.
fn tree() -> &'static Tree {
    static TREE: OnceLock<Tree> = OnceLock::new();
    TREE.get_or_init(|| {
        let files = all_files(&compilation());
        let dir = scratch("shared");
        write_tree(&dir, &files);
        let rlib = host_rlib(&dir, &["-D", "warnings"]);
        Tree { dir, rlib, files }
    })
}

fn program_path(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/programs")
        .join(name)
}

#[test]
fn generation_is_deterministic() {
    let first = all_files(&compilation());
    let second = all_files(&compilation());
    assert_eq!(first, second);
    assert!(first.len() > 20, "{} files", first.len());
}

#[test]
fn the_generated_crate_compiles_for_every_target_the_project_builds() {
    let tree = tree();
    assert!(tree.rlib.is_file(), "host build (-D warnings)");
    // The kernel's target, and userspace's code-generation target (CLAUDE.md §11).
    for (target, flags) in [
        ("x86_64-unknown-none", vec![]),
        (
            "x86_64-unknown-linux-gnu",
            vec!["-C", "target-cpu=x86-64-v3"],
        ),
    ] {
        let out = tree.dir.join(format!("{target}.rlib"));
        let mut cmd = Command::new(rustc());
        cmd.args([
            "--edition",
            "2024",
            "--crate-type",
            "rlib",
            "--crate-name",
            "oidl_generated",
        ])
        .args(["--target", target])
        .args(flags)
        .args(["-D", "warnings"])
        .arg("-o")
        .arg(&out)
        .arg(tree.dir.join("rust/lib.rs"))
        .current_dir(&tree.dir);
        run_ok(cmd, target);
    }
}

#[test]
fn the_generated_crate_passes_clippy_with_the_projects_no_panic_lints() {
    let tree = tree();
    let driver = sysroot().join("bin/clippy-driver");
    let mut cmd = Command::new(&driver);
    cmd.arg(rustc())
        .args([
            "--edition",
            "2024",
            "--crate-type",
            "rlib",
            "--crate-name",
            "oidl_generated",
        ])
        .arg(tree.dir.join("rust/lib.rs"))
        .args(["-o"])
        .arg(tree.dir.join("clippy.rlib"))
        .args(["-D", "warnings"])
        .args(["-D", "clippy::undocumented_unsafe_blocks"])
        .args(["-D", "clippy::missing_safety_doc"])
        .args(["-D", "clippy::multiple_unsafe_ops_per_block"])
        .env("CLIPPY_ARGS", "--no-deps")
        .current_dir(&tree.dir);
    run_ok(cmd, "clippy on the generated crate");
}

#[test]
fn unsafe_appears_only_in_the_named_shims() {
    let tree = tree();
    let mut checked = 0;
    for (path, text) in &tree.files {
        if !path.ends_with(".rs") || path.ends_with("corpus_check.rs") {
            continue;
        }
        // The two named shims (§11): a `syscall` client's imports, and `origin.core`'s stubs,
        // which are a tree of their own. That tree's root declares them and holds no code.
        let is_stubs_root = path == "core/lib.rs";
        let is_shim = path.ends_with("/abi.rs") || (path.starts_with("core/") && !is_stubs_root);
        // Documentation mentions `unsafe`, code does not.
        let code: String = text
            .lines()
            .filter(|line| {
                let trimmed = line.trim_start();
                !trimmed.starts_with("//") && !trimmed.starts_with("#![")
            })
            .collect::<Vec<_>>()
            .join("\n");
        if is_shim {
            assert!(code.contains("unsafe"), "{path}: expected to be a shim");
        } else {
            assert!(
                !code.contains("unsafe"),
                "{path}: unsafe outside the named shims"
            );
            assert!(
                text.contains("#![forbid(unsafe_code)]")
                    || path == "rust/lib.rs"
                    || is_stubs_root
                    || path.ends_with("/mod.rs"),
                "{path}: must forbid unsafe"
            );
        }
        checked += 1;
    }
    assert!(checked > 10, "{checked} files");
}

/// The server side alone (§11), for the crate the kernel links: the same code as the full output,
/// less the client and the two shims, and it builds with no `unsafe` under a forbidding root.
#[test]
fn the_server_side_is_the_same_code_without_the_client_or_the_shims() {
    let comp = compilation();
    let everything = all_files(&comp);
    let server = codegen::generate_server(&comp);
    let interfaces: usize = comp.libraries.iter().map(|l| l.interfaces.len()).sum();
    assert_eq!(server.len(), 2 + 2 * comp.libraries.len() + 2 * interfaces);
    for file in &server {
        let name = file.path.rsplit('/').next().expect("a name");
        assert!(
            !["client.rs", "abi.rs"].contains(&name) && !file.path.starts_with("core/"),
            "{}",
            file.path
        );
        let code: String = file
            .text
            .lines()
            .filter(|line| {
                let trimmed = line.trim_start();
                !trimmed.starts_with("//") && !trimmed.starts_with("#![")
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(!code.contains("unsafe"), "{}: unsafe", file.path);
        // Only what lists the modules differs from the full output.
        let is_interface_mod = name == "mod.rs" && file.path.matches('/').count() == 2;
        if file.path != "lib.rs" && !is_interface_mod {
            assert_eq!(
                everything.get(&format!("rust/{}", file.path)),
                Some(&file.text),
                "{}",
                file.path
            );
        }
    }
    let root = server
        .iter()
        .find(|file| file.path == "lib.rs")
        .expect("a crate root");
    assert!(
        root.text.contains("#![forbid(unsafe_code)]"),
        "{}",
        root.text
    );

    // It builds alone, every library of both transports, for the kernel's target.
    let dir = scratch("server-side");
    let files: BTreeMap<String, String> = server
        .into_iter()
        .map(|file| (file.path, file.text))
        .collect();
    write_tree(&dir, &files);
    let mut cmd = Command::new(rustc());
    cmd.args([
        "--edition",
        "2024",
        "--crate-type",
        "rlib",
        "--crate-name",
        "oidl_server",
        "--target",
        "x86_64-unknown-none",
        "-D",
        "warnings",
        "-o",
    ])
    .arg(dir.join("server.rlib"))
    .arg(dir.join("lib.rs"))
    .current_dir(&dir);
    run_ok(cmd, "the server side alone");
}

/// One channel library's crate (§11), for its services and their clients: its types, servers and
/// clients exactly as the full output writes them; of the libraries it uses, directly or not, only
/// their types; no shim and no `unsafe`; and it builds alone, for the kernel's target too.
#[test]
fn a_library_crate_is_its_server_and_client_and_the_types_it_uses() {
    const ECHO_LIBRARY: &str = "origin.fixture.echo";
    let comp = compilation();
    let everything = all_files(&comp);
    let files = codegen::generate_library(&comp, ECHO_LIBRARY).expect("a channel library");
    let echo = comp
        .libraries
        .iter()
        .find(|library| library.name == ECHO_LIBRARY)
        .expect("the fixture");
    // The fixture uses both other libraries, so each is in the crate, by its types alone.
    assert_eq!(echo.uses.len(), 2, "{:?}", echo.uses);
    assert_eq!(
        files.len(),
        2 + 2 * (echo.uses.len() + 1) + 3 * echo.interfaces.len()
    );
    for file in &files {
        let name = file.path.rsplit('/').next().expect("a name");
        assert!(
            name != "abi.rs" && !file.path.starts_with("core/"),
            "{}",
            file.path
        );
        let code: String = file
            .text
            .lines()
            .filter(|line| {
                let trimmed = line.trim_start();
                !trimmed.starts_with("//") && !trimmed.starts_with("#![")
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(!code.contains("unsafe"), "{}: unsafe", file.path);
        let full = everything.get(&format!("rust/{}", file.path));
        if file.path.starts_with("origin_fixture_echo/") || file.path == "oidl_rt.rs" {
            assert_eq!(full, Some(&file.text), "{}", file.path);
        } else if file.path != "lib.rs" {
            // A used library: its types as the full output writes them, and no interface.
            assert!(
                name == "types.rs" || name == "mod.rs",
                "{}: only types",
                file.path
            );
            if name == "types.rs" {
                assert_eq!(full, Some(&file.text), "{}", file.path);
            } else {
                assert!(
                    file.text.contains("pub mod types;")
                        && file.text.matches("pub mod ").count() == 1,
                    "{}: {}",
                    file.path,
                    file.text
                );
            }
        }
    }
    let clients = files
        .iter()
        .filter(|file| file.path.ends_with("/client.rs"))
        .count();
    assert_eq!(clients, echo.interfaces.len());
    let root = files
        .iter()
        .find(|file| file.path == "lib.rs")
        .expect("a crate root");
    assert!(
        root.text.contains("#![forbid(unsafe_code)]"),
        "{}",
        root.text
    );

    // A syscall library's client is the shims, and an unknown library is no library.
    let refused = codegen::generate_library(&comp, "origin.kernel").expect_err("syscall");
    assert!(refused.contains("syscall"), "{refused}");
    assert!(codegen::generate_library(&comp, "origin.no.such").is_err());

    // A library given but not used is left out: here one that uses only origin.kernel.
    let mut with_other = inputs();
    with_other.push(Input {
        file: "other.oidl".to_string(),
        text: b"/// o\nlibrary origin.fixture.other;\ntransport channel;\nversion 0.1;\nuse origin.kernel;\n/// i\ninterface other {\n    /// m\n    method ping = 1 () errors() requires none;\n}\n".to_vec(),
    });
    let wider = compile(&with_other, Some(VERSIONS)).expect("valid");
    let other = codegen::generate_library(&wider, "origin.fixture.other").expect("channel");
    let other_paths: Vec<&str> = other.iter().map(|file| file.path.as_str()).collect();
    assert_eq!(
        other_paths,
        [
            "lib.rs",
            "oidl_rt.rs",
            "origin_fixture_other/mod.rs",
            "origin_fixture_other/other/client.rs",
            "origin_fixture_other/other/mod.rs",
            "origin_fixture_other/other/server.rs",
            "origin_fixture_other/types.rs",
            "origin_kernel/mod.rs",
            "origin_kernel/types.rs",
        ]
    );

    // It builds alone, for the kernel's target.
    let dir = scratch("library-crate");
    let tree: BTreeMap<String, String> = files
        .into_iter()
        .map(|file| (file.path, file.text))
        .collect();
    write_tree(&dir, &tree);
    let mut cmd = Command::new(rustc());
    cmd.args([
        "--edition",
        "2024",
        "--crate-type",
        "rlib",
        "--crate-name",
        "oidl_library",
        "--target",
        "x86_64-unknown-none",
        "-D",
        "warnings",
        "-o",
    ])
    .arg(dir.join("library.rlib"))
    .arg(dir.join("lib.rs"))
    .current_dir(&dir);
    run_ok(cmd, "a library crate alone");
}

/// The C standards the headers are compiled under: from the first with `_Static_assert` to the
/// newest published (`docs/spec/OIDL.md` §17).
const C_STANDARDS: &[&str] = &["c11", "c17", "c23"];
/// The C++ standards, likewise.
const CXX_STANDARDS: &[&str] = &["c++11", "c++17", "c++20", "c++23"];

fn compiler(language: &str) -> &'static str {
    if language == "c" { "cc" } else { "c++" }
}

/// The `-std` flag this host's compiler takes for `standard`: the published name where it
/// knows it, else the provisional name older compilers use for the same standard (GCC 13, for
/// one, takes `c2x` and `c++2b` but not `c23` and `c++23`).
fn std_flag(language: &str, standard: &str) -> String {
    let provisional = match standard {
        "c23" => "c2x",
        "c++20" => "c++2a",
        "c++23" => "c++2b",
        other => other,
    };
    let accepts = |name: &str| {
        Command::new(compiler(language))
            .arg(format!("-std={name}"))
            .args(["-fsyntax-only", "-x", language, "-"])
            .stdin(std::process::Stdio::null())
            .output()
            .is_ok_and(|out| out.status.success())
    };
    if provisional == standard || accepts(standard) {
        format!("-std={standard}")
    } else {
        format!("-std={provisional}")
    }
}

/// Compiles `source` under one standard with every warning enabled, and, with `strict`, every
/// warning an error.
fn try_compile_c(
    language: &str,
    standard: &str,
    strict: bool,
    dir: &Path,
    source: &str,
    name: &str,
) -> Result<(), String> {
    let path = dir.join(name);
    fs::write(&path, source).expect("write");
    let compiler = compiler(language);
    let mut cmd = Command::new(compiler);
    cmd.args(["-Wall", "-Wextra", "-pedantic", "-fsyntax-only"])
        .args(strict.then_some("-Werror"))
        .arg(std_flag(language, standard))
        .arg("-I")
        .arg(dir.join("c"))
        .args(["-x", language])
        .arg(&path)
        .current_dir(dir);
    let out = cmd.output().map_err(|e| format!("{compiler}: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).into_owned())
    }
}

fn compile_c(language: &str, standard: &str, dir: &Path, source: &str, name: &str) {
    if let Err(message) = try_compile_c(language, standard, true, dir, source, name) {
        panic!("{language} {standard} failed:\n{message}");
    }
}

#[test]
fn the_c_headers_compile_under_every_standard_from_the_oldest_to_the_newest() {
    let tree = tree();
    let source = "#include <origin/kernel.h>\n#include <origin/fixture/echo.h>\n#include <origin/fixture/calls.h>\nint main(void) { return (int)ORIGIN_KERNEL_MAX_MESSAGE_HANDLES * 0; }\n";
    for standard in C_STANDARDS {
        compile_c("c", standard, &tree.dir, source, "headers.c");
    }
    for standard in CXX_STANDARDS {
        compile_c("c++", standard, &tree.dir, source, "headers.cpp");
    }
    // The compilers check the layout assertions; pin one, so a removed assertion is noticed.
    let kernel = &tree.files["c/origin/kernel.h"];
    assert!(kernel.contains("ORIGIN_OIDL_ASSERT(sizeof(origin_kernel_read_sizes_t) == 32"));
    assert!(kernel.contains(
        "ORIGIN_OIDL_ASSERT(offsetof(origin_kernel_read_sizes_t, sender_principal) == 24"
    ));
    assert!(
        kernel.contains("ORIGIN_OIDL_ASSERT(offsetof(origin_kernel_read_sizes_t, handles) == 8")
    );
    // channel_read's handles are records, which C passes as the generated struct (ADR-0005 B3).
    assert!(kernel.contains("origin_kernel_received_handle_t *handles, uint64_t handles_capacity"));
    // Booleans cross the C ABI as bytes (CLAUDE.md §10).
    let echo = &tree.files["c/origin/fixture/calls.h"];
    assert!(echo.contains("uint8_t e"), "{echo}");
    assert!(
        !echo.contains(" bool ") && !echo.contains("_Bool"),
        "a C bool must not appear: {echo}"
    );
}

#[test]
fn the_standards_really_change_the_language_being_compiled() {
    // The control for the test above: if a `-std` flag were ignored, that test would prove
    // nothing. `bool` is a keyword only from C23, and `consteval` only from C++20. Under the
    // older standard the identifier must compile; a compiler may still warn that a later
    // standard reserves it (GCC's -Wc++20-compat, in -Wall), so that half is not strict.
    let dir = scratch("c-standards");
    let c = "int bool = 1;\nint main(void) { return bool - 1; }\n";
    assert_eq!(try_compile_c("c", "c11", false, &dir, c, "key.c"), Ok(()));
    let error = try_compile_c("c", "c23", true, &dir, c, "key.c").expect_err("C23 reserves bool");
    assert!(error.contains("error"), "{error}");
    let cxx = "int consteval = 1;\nint main() { return consteval - 1; }\n";
    assert_eq!(
        try_compile_c("c++", "c++11", false, &dir, cxx, "key.cpp"),
        Ok(())
    );
    let error =
        try_compile_c("c++", "c++23", true, &dir, cxx, "key.cpp").expect_err("C++20 reserves it");
    assert!(error.contains("error"), "{error}");
}

#[test]
fn doc_comments_cannot_escape_into_the_generated_c() {
    // The trailing backslash would continue a line comment onto the next line of the header.
    let definition = "/// lib\nlibrary test.doc;\ntransport syscall;\nversion 1.0;\n/// A value. C:\\\nconst tricky: u32 = 7;\n/// Another.\nconst after: u32 = 8;\n";
    let comp = compile(
        &[Input {
            file: "doc".to_string(),
            text: definition.as_bytes().to_vec(),
        }],
        None,
    )
    .expect("valid");
    let files = all_files(&comp);
    let dir = scratch("doc-escape");
    write_tree(&dir, &files);
    let source = "#include <test/doc.h>\n_Static_assert(TEST_DOC_TRICKY == 7, \"tricky\");\n_Static_assert(TEST_DOC_AFTER == 8, \"after\");\nint main(void) { return 0; }\n";
    compile_c("c", "c11", &dir, source, "doc.c");
}

#[test]
fn the_corpus_passes_and_every_planted_fault_fails_it() {
    let tree = tree();
    let corpora: Vec<PathBuf> = tree
        .files
        .keys()
        .filter(|path| path.starts_with("corpus/"))
        .map(|path| tree.dir.join(path))
        .collect();
    let harness = |dir: &Path, rlib: &Path| -> Result<(), String> {
        let program = dir.join("corpus_check");
        let mut cmd = Command::new(rustc());
        cmd.args([
            "--edition",
            "2024",
            "--crate-type",
            "bin",
            "--crate-name",
            "corpus_check",
        ])
        .args(["-A", "warnings"])
        .arg("--extern")
        .arg(format!("oidl_generated={}", rlib.display()))
        .arg("-o")
        .arg(&program)
        .arg(dir.join("rust/bin/corpus_check.rs"))
        .current_dir(dir);
        run_ok(cmd, "compile the corpus runner");
        let mut run = Command::new(&program);
        run.args(&corpora);
        let out = run.output().expect("run the corpus runner");
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        if out.status.success() {
            println!("{}", stdout.trim());
            Ok(())
        } else {
            Err(format!(
                "{}\n{}",
                stdout.trim(),
                stderr.lines().next().unwrap_or("")
            ))
        }
    };
    // The control: untouched, the corpus passes.
    harness(&tree.dir, &tree.rlib).expect("the unmodified decoders pass their corpus");

    // Each fault removes one check from the generated code.
    let faults: &[(&str, &str, &str, &str)] = &[
        (
            "padding is not checked",
            "rust/oidl_rt.rs",
            "if slice_at(bytes, at, len)?.iter().all(|b| *b == 0) {",
            "if slice_at(bytes, at, len).is_ok() {",
        ),
        (
            "any non-zero byte is true",
            "rust/oidl_rt.rs",
            "[raw] => decode_bool(*raw).ok_or(WireError::Bool),",
            "[raw] => Ok(*raw != 0),",
        ),
        (
            "longer bodies are accepted",
            "rust/oidl_rt.rs",
            "if total == self.body.len() {",
            "if total <= self.body.len() {",
        ),
        (
            "the size field is ignored",
            "rust/oidl_rt.rs",
            "if usize::try_from(size).ok() != Some(message.len()) || message.len() & 7 != 0 {",
            "if false {",
        ),
        (
            "the wire revision is ignored",
            "rust/oidl_rt.rs",
            "if wire != WIRE_REVISION {",
            "if false {",
        ),
        (
            "the kind is ignored",
            "rust/oidl_rt.rs",
            "if !matches!(kind, KIND_REQUEST | KIND_RESPONSE | KIND_ERROR) {",
            "if false {",
        ),
        (
            "reserved header bytes are ignored",
            "rust/oidl_rt.rs",
            "let reserved: u16 = field(message, 6, 0)?;\n    if reserved != 0 {",
            "let reserved: u16 = field(message, 6, 0)?;\n    if reserved != reserved {",
        ),
        (
            "counts are not bounded",
            "rust/oidl_rt.rs",
            "if count > max {\n            Err(WireError::Count)",
            "if false {\n            Err(WireError::Count)",
        ),
        (
            "handle slots are not checked",
            "rust/oidl_rt.rs",
            "if usize::try_from(slot).ok() != Some(self.used) {",
            "if false {",
        ),
        (
            "handle types are not checked",
            "rust/oidl_rt.rs",
            "if object != 0 && handle.object != object {",
            "if false {",
        ),
        (
            "handle rights are not checked",
            "rust/oidl_rt.rs",
            "if handle.rights & rights != rights {",
            "if false {",
        ),
        (
            "leftover handles are accepted",
            "rust/oidl_rt.rs",
            "if self.used == self.list.len() {",
            "if true {",
        ),
        (
            "strings need not be UTF-8",
            "rust/oidl_rt.rs",
            "let bytes = slice_at(self.body, self.at, len)?;\n        let text = core::str::from_utf8(bytes).map_err(|_| WireError::Utf8)?;",
            "let bytes = slice_at(self.body, self.at, len)?;\n        let text = core::str::from_utf8(bytes).unwrap_or(\"\");",
        ),
        (
            "strings may hold NUL",
            "rust/oidl_rt.rs",
            "let text = core::str::from_utf8(bytes).map_err(|_| WireError::Utf8)?;\n        if has_nul(bytes) {",
            "let text = core::str::from_utf8(bytes).map_err(|_| WireError::Utf8)?;\n        if false {",
        ),
        (
            "vector padding is not checked",
            "rust/oidl_rt.rs",
            "check_zero(self.body, end, rounded.saturating_sub(len))?;",
            "",
        ),
        (
            "vector elements are not checked",
            "rust/oidl_rt.rs",
            "field::<T>(bytes, offset, minor)?;\n            offset",
            "offset",
        ),
        (
            "handle vectors are not consumed in order",
            "rust/oidl_rt.rs",
            "handles.take(slot, object, rights)?;\n            offset",
            "let _ = slot;\n            let _ = handles.take(0, object, rights);\n            offset",
        ),
        (
            "enums are not closed",
            "rust/origin_fixture_echo/types.rs",
            "let value = Self::from_raw(raw).ok_or(rt::WireError::Enum)?;",
            "let value = Self::from_raw(raw).unwrap_or(Self::Red);",
        ),
        (
            "a newer enum member is accepted early",
            "rust/origin_fixture_echo/types.rs",
            "if minor < value.since_minor() {",
            "if false {",
        ),
        (
            "undeclared bits are accepted",
            "rust/origin_fixture_echo/types.rs",
            "if raw & !Self::known(minor) != 0 {",
            "if false {",
        ),
        (
            "struct padding is not checked",
            "rust/origin_fixture_echo/types.rs",
            "rt::check_zero(oidl_bytes, 1, 3)?;",
            "",
        ),
        (
            "message padding is not checked",
            "rust/origin_fixture_echo/echo/server.rs",
            "oidl_r.zero(1, 1)?;",
            "",
        ),
        (
            "@nonzero is not checked",
            "rust/origin_fixture_echo/echo/server.rs",
            "if nz == 0 {",
            "if false {",
        ),
        (
            "the capability gate is open",
            "rust/origin_kernel/kernel/server.rs",
            "if !oidl_calls.holds(Capability::KernelDebug) {",
            "if false {",
        ),
        (
            "a register value may be wider than its type",
            "rust/origin_fixture_calls/calls/server.rs",
            "u32::try_from(oidl_a2).map_err(|_| ScalarsError::InvalidArgs)?",
            "oidl_a2 as u32",
        ),
        (
            "a register bool is coerced",
            "rust/origin_fixture_calls/calls/server.rs",
            "u8::try_from(oidl_a4).ok().and_then(rt::decode_bool).ok_or(ScalarsError::InvalidArgs)?",
            "(oidl_a4 != 0)",
        ),
        (
            "a register enum is not closed",
            "rust/origin_fixture_calls/calls/server.rs",
            "and_then(Kind::from_raw).ok_or(ScalarsError::InvalidArgs)?",
            "and_then(Kind::from_raw).unwrap_or(Kind::One)",
        ),
        (
            "a later result is read at an older minor",
            "rust/origin_fixture_echo/echo/server.rs",
            "let oidl_count_note = if oidl_minor >= 1 {",
            "let oidl_count_note = if true {",
        ),
        (
            "another library's members are gated by this library's minor",
            "rust/origin_fixture_echo/echo/server.rs",
            "let m: super::super::super::origin_fixture_calls::Mode = oidl_r.field(0, u16::MAX)?;",
            "let m: super::super::super::origin_fixture_calls::Mode = oidl_r.field(0, oidl_minor)?;",
        ),
        (
            "a pointer's count is not bounded",
            "rust/oidl_rt.rs",
            "if count > u64::from(max) {\n            return None;",
            "if false {\n            return None;",
        ),
        (
            "a register bits value may be zero",
            "rust/origin_kernel/kernel/server.rs",
            "if signals.bits() == 0 {",
            "if false {",
        ),
        (
            "undeclared register bits are accepted",
            "rust/origin_kernel/types.rs",
            "if raw & !Self::ALL.0 == 0 {",
            "if true {",
        ),
        // origin.kernel has no reserved call since 0.6 (ADR-0005 Amendment 7): the fixture's.
        (
            "a reserved call succeeds",
            "rust/origin_fixture_calls/calls/server.rs",
            "number::UNIMPLEMENTED => super::super::super::origin_kernel::Error::NotSupported.raw(),",
            "number::UNIMPLEMENTED => 0,",
        ),
    ];
    let results: Mutex<Vec<(String, Result<String, String>)>> = Mutex::new(Vec::new());
    let next = Mutex::new(0usize);
    let threads = std::thread::available_parallelism().map_or(2, |n| n.get().min(4));
    std::thread::scope(|scope| {
        for _ in 0..threads {
            scope.spawn(|| {
                loop {
                    let index = {
                        let mut next = next.lock().expect("lock");
                        let index = *next;
                        *next += 1;
                        index
                    };
                    let Some((what, file, find, replace)) = faults.get(index).copied() else {
                        return;
                    };
                    let outcome = (|| -> Result<String, String> {
                        let mut files = tree.files.clone();
                        let text = files
                            .get_mut(file)
                            .ok_or_else(|| format!("no file {file}"))?;
                        if !text.contains(find) {
                            return Err(format!("the pattern to break is gone: {find}"));
                        }
                        *text = text.replacen(find, replace, 1);
                        let dir = scratch(&format!("fault-{index}"));
                        write_tree(&dir, &files);
                        let rlib = host_rlib(&dir, &["-A", "warnings"]);
                        match harness(&dir, &rlib) {
                            Ok(()) => Err("the corpus still passes".to_string()),
                            Err(report) => Ok(report),
                        }
                    })();
                    results
                        .lock()
                        .expect("lock")
                        .push((what.to_string(), outcome));
                }
            });
        }
    });
    let results = results.into_inner().expect("lock");
    assert_eq!(results.len(), faults.len());
    let mut failed = Vec::new();
    for (what, outcome) in &results {
        match outcome {
            Ok(report) => println!("fault detected: {what}: {report}"),
            Err(why) => failed.push(format!("{what}: {why}")),
        }
    }
    assert!(
        failed.is_empty(),
        "undetected faults:\n{}",
        failed.join("\n")
    );
    println!("{} faults, all detected by the corpus", faults.len());
}

#[test]
fn the_fixture_api_round_trips_and_the_server_refuses_what_it_must() {
    let tree = tree();
    let out = run_program(
        &tree.dir,
        &tree.rlib,
        &program_path("fixture_api.rs"),
        "fixture_api",
        &["-A", "warnings"],
    );
    assert!(out.contains("all checks passed"), "{out}");
}

/// A copy of the shared tree. `syscall_api.rs` defines the C entry points itself, a mock kernel;
/// the real ones are `origin.core`'s, under `core/`, and the client's crate under `rust/` holds
/// none of them, so nothing has to be taken out for an x86-64 host.
fn syscall_api_files() -> BTreeMap<String, String> {
    tree().files.clone()
}

/// The negative control of `syscall_api.rs`'s checks that a handle reaches the kernel whole
/// (ADR-0005 Amendment 6, E1; task S4-0): a decoder planted to keep only a handle register's low
/// 32 bits must fail them. The corpus cannot see this, since nothing in it is refused.
#[test]
fn a_handle_cut_to_32_bits_fails_the_syscall_checks() {
    let mut files = syscall_api_files();
    let runtime = files.get_mut("rust/oidl_rt.rs").expect("the runtime");
    let find = "            value: raw,\n            need_rights,\n";
    assert!(runtime.contains(find), "the pattern to break is gone");
    *runtime = runtime.replacen(
        find,
        "            value: raw & 0xFFFF_FFFF,\n            need_rights,\n",
        1,
    );
    let dir = scratch("syscall-api-cut");
    write_tree(&dir, &files);
    let rlib = host_rlib(&dir, &["-A", "warnings"]);
    let program = dir.join("syscall_api");
    let mut cmd = Command::new(rustc());
    cmd.args([
        "--edition",
        "2024",
        "--crate-type",
        "bin",
        "--crate-name",
        "syscall_api",
    ])
    .arg("--extern")
    .arg(format!("oidl_generated={}", rlib.display()))
    .arg("-o")
    .arg(&program)
    .arg(program_path("syscall_api.rs"))
    .args(["-A", "warnings"])
    .current_dir(&dir);
    run_ok(cmd, "compile syscall_api with the planted cut");
    let out = Command::new(&program).output().expect("run syscall_api");
    assert!(
        !out.status.success(),
        "a handle cut to 32 bits passed every check: {}",
        String::from_utf8_lossy(&out.stdout)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("a handle's top half was lost"),
        "failed elsewhere: {stderr}"
    );
}

#[test]
fn the_syscall_outputs_decode_exactly_and_the_client_works_through_the_c_abi() {
    let files = syscall_api_files();
    let dir = scratch("syscall-api");
    write_tree(&dir, &files);
    let rlib = host_rlib(&dir, &["-A", "warnings"]);
    let out = run_program(
        &dir,
        &rlib,
        &program_path("syscall_api.rs"),
        "syscall_api",
        &["-A", "warnings"],
    );
    assert!(out.contains("all checks passed"), "{out}");
}

/// The toolchain's `llvm-objdump`, for the host this test runs on.
fn objdump() -> PathBuf {
    let out = Command::new(rustc()).arg("-vV").output().expect("rustc");
    let host = String::from_utf8(out.stdout)
        .expect("utf8")
        .lines()
        .find_map(|l| l.strip_prefix("host: ").map(str::to_string))
        .expect("host");
    sysroot()
        .join("lib/rustlib")
        .join(host)
        .join("bin/llvm-objdump")
}

/// Compiles the stubs' crate, `core/lib.rs` under `dir`, for the userspace triple with its
/// flags, and returns its disassembly in Intel syntax.
fn stubs_listing(dir: &Path) -> String {
    let object = dir.join("stubs.o");
    let mut cmd = Command::new(rustc());
    cmd.args([
        "--edition",
        "2024",
        "--crate-type",
        "rlib",
        "--crate-name",
        "origin_core_stubs",
    ])
    .args(["--target", "x86_64-unknown-linux-gnu"])
    .args([
        "-C",
        "target-cpu=x86-64-v3",
        "-C",
        "opt-level=2",
        "-C",
        "codegen-units=1",
    ])
    .args(["-D", "warnings"])
    .arg("--emit=obj")
    .arg("-o")
    .arg(&object)
    .arg(dir.join("core/lib.rs"))
    .current_dir(dir);
    run_ok(cmd, "compile the stubs");
    let mut cmd = Command::new(objdump());
    cmd.args(["-d", "-z", "--no-show-raw-insn", "--x86-asm-syntax=intel"])
        .arg(&object);
    run_ok(cmd, "llvm-objdump")
}

/// The instructions of the function `symbol` in `listing`, each as the generator spells it: one
/// space after the mnemonic.
fn listed(listing: &str, symbol: &str) -> Vec<String> {
    let header = format!("<{symbol}>:\n");
    let start = listing
        .find(&header)
        .unwrap_or_else(|| panic!("no symbol {symbol} in the object:\n{listing}"));
    listing[start + header.len()..]
        .lines()
        .take_while(|line| !line.trim().is_empty())
        .map(|line| {
            let (_, instruction) = line.split_once(':').expect("an address, then a colon");
            instruction.trim().replace('\t', " ")
        })
        .collect()
}

/// How `llvm-objdump` prints the load of call number `number`.
fn number_load(number: u32) -> String {
    format!("mov eax, {number:#x}")
}

/// Each stub, as it was assembled, is what §10 says and nothing more: the extension of each of
/// its own narrow parameters, the fourth argument's move, the load of its own number, one
/// `syscall`, and the return (ADR-0005 B2; ADR-0011 L6 and L13).
#[test]
fn each_stub_is_its_extensions_its_own_number_one_syscall_and_the_return() {
    let tree = tree();
    let listing = stubs_listing(&tree.dir);
    let comp = compilation();
    let mut checked = 0;
    for library in &comp.libraries {
        if library.transport != oidl::model::Transport::Syscall {
            continue;
        }
        let prefix = library.name.replace('.', "_");
        for method in library.interfaces.iter().flat_map(|i| &i.methods) {
            let symbol = format!("{prefix}_{}", method.name);
            let got = listed(&listing, &symbol);
            let mut wanted = codegen::rust::stub_instructions(method);
            let load = wanted.len() - 3;
            assert_eq!(wanted[load], format!("mov eax, {}", method.ordinal));
            wanted[load] = number_load(method.ordinal);
            assert_eq!(got, wanted, "{symbol}");
            // And by itself, whatever the generator's own list says.
            assert_eq!(
                got.iter().filter(|i| i.as_str() == "syscall").count(),
                1,
                "{symbol}: exactly one syscall instruction"
            );
            let at = got.iter().position(|i| i == "syscall").expect("a syscall");
            assert_eq!(got[at - 1], number_load(method.ordinal), "{symbol}");
            assert_eq!(
                got[at + 1..],
                [if method.never { "ud2" } else { "ret" }],
                "{symbol}"
            );
            checked += 1;
        }
    }
    assert!(checked >= 30, "{checked} stubs");
    // The fixture's `scalars`: a byte, a signed 16-bit value, 32 bits, 64 bits in the fourth
    // register, a boolean and a signed 32-bit enumeration (L13).
    assert_eq!(
        listed(&listing, "origin_fixture_calls_scalars"),
        [
            "movzx edi, dil",
            "movsx rsi, si",
            "mov edx, edx",
            "mov r10, rcx",
            "movzx r8d, r8b",
            "movsxd r9, r9d",
            "mov eax, 0x1",
            "syscall",
            "ret",
        ]
    );
}

/// A definition whose methods take six parameters of one narrow type each: every type in every
/// register.
fn widen_definition() -> String {
    let mut text = String::from(
        "/// Fixture: narrow parameters.\nlibrary origin.fixture.widen;\ntransport syscall;\nversion 1.0;\nuse origin.kernel;\n\
         /// Flags.\nbits flags: u16 {\n    /// A bit.\n    one = 0;\n}\n\
         /// A choice.\nenum choice: u8 {\n    /// One.\n    one = 1;\n}\n\
         /// Calls.\ninterface widen {\n",
    );
    for (number, ty) in [
        "u8", "u16", "u32", "i8", "i16", "i32", "bool", "flags", "choice", "u64",
    ]
    .iter()
    .enumerate()
    {
        let params: Vec<String> = ["a", "b", "c", "d", "e", "f"]
            .iter()
            .map(|name| format!("        /// One.\n        {name}: {ty},\n"))
            .collect();
        text.push_str(&format!(
            "    /// Six of `{ty}`.\n    method six_{ty} = {} (\n{}    ) errors(invalid_args) requires none;\n",
            number + 1,
            params.concat()
        ));
    }
    text.push_str("}\n");
    text
}

/// L13, for each narrow type in each of the six registers, as the assembler wrote it: zero for an
/// unsigned integer, a boolean, a bits type and an unsigned enumeration; the sign for a signed
/// integer; the fourth argument through R10. The table is written out here by hand, apart from
/// the generator's own.
#[test]
fn a_stub_extends_each_narrow_type_in_each_register() {
    let inputs = vec![
        Input {
            file: "origin.kernel.oidl".to_string(),
            text: KERNEL.as_bytes().to_vec(),
        },
        Input {
            file: "widen.oidl".to_string(),
            text: widen_definition().into_bytes(),
        },
    ];
    let comp = compile(&inputs, Some(VERSIONS)).expect("the fixture is valid");
    let dir = scratch("widen");
    write_tree(&dir, &all_files(&comp));
    let listing = stubs_listing(&dir);
    let byte = [
        "movzx edi, dil",
        "movzx esi, sil",
        "movzx edx, dl",
        "movzx r10d, cl",
        "movzx r8d, r8b",
        "movzx r9d, r9b",
    ];
    let word = [
        "movzx edi, di",
        "movzx esi, si",
        "movzx edx, dx",
        "movzx r10d, cx",
        "movzx r8d, r8w",
        "movzx r9d, r9w",
    ];
    let double = [
        "mov edi, edi",
        "mov esi, esi",
        "mov edx, edx",
        "mov r10d, ecx",
        "mov r8d, r8d",
        "mov r9d, r9d",
    ];
    let signed_byte = [
        "movsx rdi, dil",
        "movsx rsi, sil",
        "movsx rdx, dl",
        "movsx r10, cl",
        "movsx r8, r8b",
        "movsx r9, r9b",
    ];
    let signed_word = [
        "movsx rdi, di",
        "movsx rsi, si",
        "movsx rdx, dx",
        "movsx r10, cx",
        "movsx r8, r8w",
        "movsx r9, r9w",
    ];
    let signed_double = [
        "movsxd rdi, edi",
        "movsxd rsi, esi",
        "movsxd rdx, edx",
        "movsxd r10, ecx",
        "movsxd r8, r8d",
        "movsxd r9, r9d",
    ];
    let cases: [(&str, &[&str]); 10] = [
        ("u8", &byte),
        ("u16", &word),
        ("u32", &double),
        ("i8", &signed_byte),
        ("i16", &signed_word),
        ("i32", &signed_double),
        ("bool", &byte),
        ("flags", &word),
        ("choice", &byte),
        // A value that fills its register is passed as it is: only the fourth moves.
        ("u64", &["mov r10, rcx"]),
    ];
    for (number, (ty, extensions)) in cases.iter().enumerate() {
        let mut wanted: Vec<String> = extensions.iter().map(|i| i.to_string()).collect();
        wanted.push(number_load(number as u32 + 1));
        wanted.push("syscall".to_string());
        wanted.push("ret".to_string());
        assert_eq!(
            listed(&listing, &format!("origin_fixture_widen_six_{ty}")),
            wanted,
            "six parameters of {ty}"
        );
    }
}

/// The client's crate holds no stub: a program that links it holds no `syscall` of its own, and
/// reaches the kernel through `origin.core`'s entry points alone (ADR-0011 L3, L6).
#[test]
fn the_clients_tree_holds_no_stub() {
    let tree = tree();
    let mut checked = 0;
    for (path, text) in &tree.files {
        if !path.starts_with("rust/") {
            continue;
        }
        assert!(
            !text.contains("asm!") && !text.contains("naked"),
            "{path}: assembly in the client's tree"
        );
        checked += 1;
    }
    assert!(checked > 10, "{checked} files");
    let stubs = &tree.files["core/origin_kernel/kernel.rs"];
    assert_eq!(
        stubs.matches("\"syscall\",").count(),
        compilation()
            .libraries
            .iter()
            .find(|l| l.name == "origin.kernel")
            .expect("kernel")
            .interfaces[0]
            .methods
            .len(),
        "one `syscall` a method in the stubs' file"
    );
}

/// The list of `origin.core`'s exports names every method once, with its number, its C symbol and
/// the version it arrived in, under the definition's release fingerprint (ADR-0011 L5, L7).
#[test]
fn the_list_of_exports_names_every_method_with_its_number_and_version() {
    let tree = tree();
    let comp = compilation();
    let kernel = comp
        .libraries
        .iter()
        .find(|l| l.name == "origin.kernel")
        .expect("kernel");
    let list = &tree.files["core/origin_kernel.exports"];
    let mut lines = list.lines();
    assert_eq!(
        lines.next(),
        Some(format!("# {} origin.kernel", codegen::rust::EXPORTS_FORMAT).as_str())
    );
    let release = fingerprint::fingerprints(kernel)
        .last()
        .expect("a minor")
        .release;
    assert_eq!(
        lines.next(),
        Some(format!("release sha256:{}", fingerprint::hex(&release)).as_str())
    );
    let methods = &kernel.interfaces[0].methods;
    let rest: Vec<&str> = lines.collect();
    assert_eq!(rest.len(), methods.len());
    for (line, method) in rest.iter().zip(methods) {
        assert_eq!(
            *line,
            format!(
                "{} origin_kernel_{} {} {}.{}",
                method.ordinal, method.name, method.name, method.since.major, method.since.minor
            )
        );
    }
    // Calls that arrived with a later minor say so: the memory calls came with 0.5.
    assert!(
        rest.contains(&"12 origin_kernel_memory_map memory_map 0.5"),
        "{list}"
    );
    // The same fingerprint is in the kernel's side, so the two can be compared at build time.
    let server = &tree.files["rust/origin_kernel/kernel/server.rs"];
    let bytes: Vec<String> = release.iter().map(|b| format!("{b:#04x},")).collect();
    for row in bytes.chunks(8) {
        assert!(server.contains(&row.join(" ")), "{}", row.join(" "));
    }
}

/// The kernel's pin, run on the host: every call is accepted at its own offset and at no other
/// call's, and a number with no stub nowhere (ADR-0005 B2; ADR-0011 L7).
#[test]
fn the_pin_accepts_each_number_at_its_own_offset_alone() {
    let tree = tree();
    let comp = compilation();
    let kernel = comp
        .libraries
        .iter()
        .find(|l| l.name == "origin.kernel")
        .expect("kernel");
    let methods = &kernel.interfaces[0].methods;
    let fields: Vec<String> = methods
        .iter()
        .enumerate()
        .map(|(i, m)| format!("        {}: {},\n", m.name, 0x40 + 0x10 * i))
        .collect();
    let numbers: Vec<String> = methods.iter().map(|m| m.ordinal.to_string()).collect();
    let source = format!(
        r#"use oidl_generated::origin_kernel::kernel::server::{{Pins, pinned}};

fn main() {{
    let pins = Pins {{
{}    }};
    let numbers: [u64; {count}] = [{}];
    let each = pins.each();
    assert_eq!(each.len(), numbers.len());
    for (i, &(number, offset)) in each.iter().enumerate() {{
        assert_eq!(number, numbers[i]);
        assert_eq!(offset, 0x40 + 0x10 * i as u64);
        for &(other, there) in &each {{
            assert_eq!(pinned(&pins, number, there), other == number, "{{number}} at {{there:#x}}");
        }}
        // Not one byte before or after, nor at the image's base.
        assert!(!pinned(&pins, number, offset - 1) && !pinned(&pins, number, offset + 1));
        assert!(!pinned(&pins, number, 0) && !pinned(&pins, number, u64::MAX));
    }}
    // A number with no stub is accepted nowhere, an existing stub's place included.
    for unknown in [{count}u64, 9999, 1 << 32, u64::MAX] {{
        assert!(!numbers.contains(&unknown));
        for &(_, offset) in &each {{
            assert!(!pinned(&pins, unknown, offset), "{{unknown}} at {{offset:#x}}");
        }}
    }}
    println!("all checks passed");
}}
"#,
        fields.concat(),
        numbers.join(", "),
        count = methods.len(),
    );
    let dir = scratch("pin");
    let program = dir.join("pin_check.rs");
    fs::write(&program, source).expect("write the program");
    let out = run_program(
        &tree.dir,
        &tree.rlib,
        &program,
        "pin_check",
        &["-D", "warnings"],
    );
    assert!(out.contains("all checks passed"), "{out}");
}

fn map_of(output: &str) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    for line in output.lines() {
        if let Some((key, value)) = line.rsplit_once(' ') {
            map.insert(key.to_string(), value.to_string());
        }
    }
    map
}

#[test]
fn the_generated_values_equal_the_hand_written_abi() {
    use origin_abi::{
        Error, OK, ObjectType, limits, read_sizes, received_handle, rights, signals, syscall,
    };
    let tree = tree();
    let out = run_program(
        &tree.dir,
        &tree.rlib,
        &program_path("kernel_values.rs"),
        "kernel_values",
        &["-A", "warnings"],
    );
    let map = map_of(&out);
    let (major, minor) = origin_abi::ABI_VERSION;
    assert!(out.contains(&format!("version {major} {minor}\n")), "{out}");
    assert_eq!(map["ok"], OK.to_string());
    for error in Error::ALL {
        assert_eq!(
            map[&format!("status {}", error.name())],
            error.raw().to_string(),
            "{:?}",
            error
        );
    }
    assert_eq!(
        map.get("status_name OK"),
        None,
        "status_name lines are ok, not status"
    );
    assert!(out.contains("status_name OK\n"));
    assert!(out.contains("status_name UNKNOWN\n"));
    assert!(out.contains("status_name LIMIT\n"));
    let right = |name: &str| map[&format!("right {name}")].clone();
    assert_eq!(right("READ"), rights::READ.to_string());
    assert_eq!(right("WRITE"), rights::WRITE.to_string());
    assert_eq!(right("MAP"), rights::MAP.to_string());
    assert_eq!(right("EXECUTE"), rights::EXECUTE.to_string());
    assert_eq!(right("DUPLICATE"), rights::DUPLICATE.to_string());
    assert_eq!(right("TRANSFER"), rights::TRANSFER.to_string());
    assert_eq!(right("WAIT"), rights::WAIT.to_string());
    assert_eq!(right("SIGNAL"), rights::SIGNAL.to_string());
    assert_eq!(right("MANAGE"), rights::MANAGE.to_string());
    assert_eq!(right("ALL"), rights::ALL.to_string());
    assert_eq!(right("CHANNEL"), rights::CHANNEL.to_string());
    let signal = |name: &str| map[&format!("signal {name}")].clone();
    assert_eq!(signal("READABLE"), signals::READABLE.to_string());
    assert_eq!(signal("WRITABLE"), signals::WRITABLE.to_string());
    assert_eq!(signal("PEER_CLOSED"), signals::PEER_CLOSED.to_string());
    assert_eq!(signal("SIGNALED"), signals::SIGNALED.to_string());
    assert_eq!(signal("TERMINATED"), signals::TERMINATED.to_string());
    assert_eq!(signal("ALL"), signals::ALL.to_string());
    let object = |name: &str| map[&format!("object {name}")].clone();
    assert_eq!(object("PROCESS"), (ObjectType::Process as u32).to_string());
    assert_eq!(object("THREAD"), (ObjectType::Thread as u32).to_string());
    assert_eq!(
        object("MEMORY_OBJECT"),
        (ObjectType::MemoryObject as u32).to_string()
    );
    assert_eq!(object("CHANNEL"), (ObjectType::Channel as u32).to_string());
    assert_eq!(object("EVENT"), (ObjectType::Event as u32).to_string());
    assert_eq!(object("PORT"), (ObjectType::Port as u32).to_string());
    assert_eq!(object("TIMER"), (ObjectType::Timer as u32).to_string());
    let number = |name: &str| map[&format!("number {name}")].clone();
    assert_eq!(number("THREAD_EXIT"), syscall::THREAD_EXIT.to_string());
    assert_eq!(number("DEBUG_REPORT"), syscall::DEBUG_REPORT.to_string());
    assert_eq!(number("DEBUG_WRITE"), syscall::DEBUG_WRITE.to_string());
    assert_eq!(number("HANDLE_CLOSE"), syscall::HANDLE_CLOSE.to_string());
    assert_eq!(
        number("HANDLE_DUPLICATE"),
        syscall::HANDLE_DUPLICATE.to_string()
    );
    assert_eq!(
        number("CHANNEL_CREATE"),
        syscall::CHANNEL_CREATE.to_string()
    );
    assert_eq!(number("CHANNEL_WRITE"), syscall::CHANNEL_WRITE.to_string());
    assert_eq!(number("CHANNEL_READ"), syscall::CHANNEL_READ.to_string());
    assert_eq!(number("WAIT_ONE"), syscall::WAIT_ONE.to_string());
    assert_eq!(number("EVENT_CREATE"), syscall::EVENT_CREATE.to_string());
    assert_eq!(number("OBJECT_SIGNAL"), syscall::OBJECT_SIGNAL.to_string());
    assert_eq!(
        map["limit MAX_MESSAGE_BYTES"],
        limits::MAX_MESSAGE_BYTES.to_string()
    );
    assert_eq!(
        map["limit MAX_MESSAGE_HANDLES"],
        limits::MAX_MESSAGE_HANDLES.to_string()
    );
    assert_eq!(
        map["limit DEADLINE_INFINITE"],
        limits::DEADLINE_INFINITE.to_string()
    );
    assert_eq!(map["read_sizes SIZE"], read_sizes::SIZE.to_string());
    assert_eq!(map["read_sizes SIZE_AT"], read_sizes::SIZE_AT.to_string());
    assert_eq!(map["read_sizes BYTES_AT"], read_sizes::BYTES_AT.to_string());
    assert_eq!(
        map["read_sizes HANDLES_AT"],
        read_sizes::HANDLES_AT.to_string()
    );
    assert_eq!(
        map["read_sizes BASE_SIZE"],
        read_sizes::BASE_SIZE.to_string()
    );
    assert_eq!(
        map["read_sizes SENDER_PROCESS_AT"],
        read_sizes::SENDER_PROCESS_AT.to_string()
    );
    assert_eq!(
        map["read_sizes SENDER_PRINCIPAL_AT"],
        read_sizes::SENDER_PRINCIPAL_AT.to_string()
    );
    assert_eq!(
        map["received_handle SIZE"],
        received_handle::SIZE.to_string()
    );
    assert_eq!(
        map["received_handle HANDLE_AT"],
        received_handle::HANDLE_AT.to_string()
    );
    assert_eq!(
        map["received_handle OBJECT_TYPE_AT"],
        received_handle::OBJECT_TYPE_AT.to_string()
    );
    assert_eq!(
        map["received_handle RIGHTS_AT"],
        received_handle::RIGHTS_AT.to_string()
    );
    assert_eq!(
        map["handle INVALID"],
        origin_abi::INVALID_HANDLE.to_string()
    );
    assert_eq!(map["bool 0"], "false");
    assert_eq!(map["bool 1"], "true");
    assert!(!map.contains_key("bool 2"));
    for raw in 0..=255u8 {
        assert_eq!(
            origin_abi::decode_bool(raw).map(|b| b.to_string()),
            map.get(&format!("bool {raw}")).cloned(),
            "byte {raw:#x}"
        );
    }
}

#[test]
fn the_definition_has_exactly_the_hand_written_abi_s_statuses_rights_signals_and_numbers() {
    use origin_abi::{Error, ObjectType, syscall};
    let comp = compilation();
    let status = comp.status.as_ref().expect("status");
    let declared: Vec<i128> = status.members.iter().map(|m| m.value).collect();
    let mut expected = vec![0i128];
    expected.extend(Error::ALL.iter().map(|e| i128::from(e.raw())));
    assert_eq!(declared, expected, "every status, in value order");
    for (member, error) in status.members.iter().skip(1).zip(Error::ALL) {
        assert_eq!(member.name.to_ascii_uppercase(), error.name());
    }
    let kernel = comp
        .libraries
        .iter()
        .find(|l| l.name == "origin.kernel")
        .expect("kernel");
    let numbers: BTreeMap<&str, u32> = kernel.interfaces[0]
        .methods
        .iter()
        .map(|m| (m.name.as_str(), m.ordinal))
        .collect();
    assert_eq!(numbers.len(), 20);
    assert_eq!(u64::from(numbers["thread_exit"]), syscall::THREAD_EXIT);
    assert_eq!(u64::from(numbers["debug_report"]), syscall::DEBUG_REPORT);
    assert_eq!(u64::from(numbers["debug_write"]), syscall::DEBUG_WRITE);
    assert_eq!(u64::from(numbers["handle_close"]), syscall::HANDLE_CLOSE);
    assert_eq!(
        u64::from(numbers["handle_duplicate"]),
        syscall::HANDLE_DUPLICATE
    );
    assert_eq!(
        u64::from(numbers["channel_create"]),
        syscall::CHANNEL_CREATE
    );
    assert_eq!(u64::from(numbers["channel_write"]), syscall::CHANNEL_WRITE);
    assert_eq!(u64::from(numbers["channel_read"]), syscall::CHANNEL_READ);
    assert_eq!(u64::from(numbers["wait_one"]), syscall::WAIT_ONE);
    assert_eq!(u64::from(numbers["event_create"]), syscall::EVENT_CREATE);
    assert_eq!(u64::from(numbers["object_signal"]), syscall::OBJECT_SIGNAL);
    assert_eq!(
        u64::from(numbers["memory_object_create"]),
        syscall::MEMORY_OBJECT_CREATE
    );
    assert_eq!(u64::from(numbers["memory_map"]), syscall::MEMORY_MAP);
    assert_eq!(u64::from(numbers["memory_unmap"]), syscall::MEMORY_UNMAP);
    assert_eq!(
        u64::from(numbers["memory_protect"]),
        syscall::MEMORY_PROTECT
    );
    assert_eq!(u64::from(numbers["port_create"]), syscall::PORT_CREATE);
    assert_eq!(
        u64::from(numbers["object_wait_async"]),
        syscall::OBJECT_WAIT_ASYNC
    );
    assert_eq!(u64::from(numbers["port_wait"]), syscall::PORT_WAIT);
    assert_eq!(u64::from(numbers["port_cancel"]), syscall::PORT_CANCEL);
    assert_eq!(u64::from(numbers["clock_read"]), syscall::CLOCK_READ);
    let object_type = comp.object_type.as_ref().expect("object_type");
    assert_eq!(
        object_type
            .members
            .iter()
            .map(|m| m.value)
            .collect::<Vec<_>>(),
        vec![
            i128::from(ObjectType::Process as u32),
            i128::from(ObjectType::Thread as u32),
            i128::from(ObjectType::MemoryObject as u32),
            i128::from(ObjectType::Channel as u32),
            i128::from(ObjectType::Event as u32),
            i128::from(ObjectType::Port as u32),
            i128::from(ObjectType::Timer as u32),
        ]
    );
    // The version comes from origin-version.toml, the one place it is written (§62).
    assert_eq!(
        (kernel.version.major, kernel.version.minor),
        (origin_abi::ABI_VERSION.0, origin_abi::ABI_VERSION.1)
    );
    let channel_rights = kernel
        .consts
        .iter()
        .find(|c| c.name == "channel_rights")
        .expect("channel_rights");
    assert!(matches!(channel_rights.kind, ConstKind::Bits(_)));
    assert_eq!(
        channel_rights.value,
        i128::from(origin_abi::rights::CHANNEL)
    );
}

#[test]
fn the_generated_decoders_match_the_hand_written_kernels_argument_handling() {
    let tree = tree();
    let out = run_program(
        &tree.dir,
        &tree.rlib,
        &program_path("kernel_differential.rs"),
        "kernel_differential",
        &["-C", "opt-level=3", "-A", "warnings"],
    );
    println!("{}", out.trim());
    assert!(out.contains("0 mismatches"), "{out}");
}

#[test]
fn rustdoc_finds_nothing_to_run_in_the_generated_crate() {
    let tree = tree();
    let rustdoc = sysroot().join("bin/rustdoc");
    let mut cmd = Command::new(rustdoc);
    cmd.args([
        "--edition",
        "2024",
        "--crate-name",
        "oidl_generated",
        "--test",
    ])
    .arg(tree.dir.join("rust/lib.rs"))
    .current_dir(&tree.dir);
    let out = run_ok(cmd, "rustdoc --test on the generated crate");
    assert!(out.contains("running 0 tests"), "{out}");
}

/// The tool's own command line (`src/main.rs`).
struct Cli {
    dir: PathBuf,
    kernel: PathBuf,
    calls: PathBuf,
    echo: PathBuf,
    versions: PathBuf,
}

impl Cli {
    fn new(name: &str) -> Self {
        let dir = scratch(name);
        let write = |file: &str, text: &str| {
            let path = dir.join(file);
            fs::write(&path, text).expect("write");
            path
        };
        Self {
            kernel: write("kernel.oidl", KERNEL),
            calls: write("calls.oidl", CALLS),
            echo: write("echo.oidl", ECHO),
            versions: write("versions.toml", VERSIONS),
            dir,
        }
    }

    fn run(&self, args: &[&str]) -> (bool, String) {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_oidl"));
        cmd.args(args).current_dir(&self.dir);
        let out = cmd.output().expect("run oidl");
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        (out.status.success(), text)
    }

    fn all(&self) -> Vec<String> {
        vec![
            self.kernel.display().to_string(),
            self.calls.display().to_string(),
            self.echo.display().to_string(),
        ]
    }
}

#[test]
fn the_command_line_checks_generates_and_guards_the_lock() {
    let cli = Cli::new("cli");
    let versions = cli.versions.display().to_string();
    let mut args = vec!["check", "--versions", &versions];
    let files = cli.all();
    args.extend(files.iter().map(String::as_str));
    let (ok, text) = cli.run(&args);
    assert!(ok, "{text}");
    assert!(text.contains("3 libraries valid"), "{text}");

    let out_dir = cli.dir.join("out");
    let out = out_dir.display().to_string();
    let mut args = vec!["generate", "--versions", &versions, "--out", &out];
    args.extend(files.iter().map(String::as_str));
    let (ok, text) = cli.run(&args);
    assert!(ok, "{text}");
    assert!(text.contains("files written"), "{text}");
    assert!(out_dir.join("rust/oidl_rt.rs").is_file());
    assert!(out_dir.join("c/origin/kernel.h").is_file());
    assert!(out_dir.join("abi/origin.kernel.fingerprint").is_file());
    assert!(out_dir.join("corpus/origin.kernel.corpus").is_file());

    // The server side alone: a crate at the top of the directory, and nothing else.
    let server_dir = cli.dir.join("server");
    let server_out = server_dir.display().to_string();
    let mut args = vec![
        "generate",
        "--server-only",
        "--versions",
        &versions,
        "--out",
        &server_out,
    ];
    args.extend(files.iter().map(String::as_str));
    let (ok, text) = cli.run(&args);
    assert!(ok, "{text}");
    assert!(server_dir.join("lib.rs").is_file());
    assert!(server_dir.join("origin_kernel/kernel/server.rs").is_file());
    for absent in [
        "rust",
        "c",
        "abi",
        "corpus",
        "doc",
        "origin_kernel/kernel/abi.rs",
    ] {
        assert!(!server_dir.join(absent).exists(), "{absent}");
    }

    // A missing lock is an error until it is created with --update.
    let lock = cli.dir.join("oidl.lock").display().to_string();
    let run_lock = |extra: &[&str]| {
        let mut args = vec!["fingerprint", "--versions", &versions, "--lock", &lock];
        args.extend_from_slice(extra);
        args.extend(files.iter().map(String::as_str));
        cli.run(&args)
    };
    let (ok, text) = run_lock(&[]);
    assert!(!ok, "a missing lock must fail: {text}");
    let (ok, text) = run_lock(&["--update"]);
    assert!(ok, "{text}");
    let (ok, text) = run_lock(&[]);
    assert!(ok, "an unchanged definition passes: {text}");

    // A changed definition at a stable major fails, and --update refuses.
    let changed = ECHO.replace("x: u8;", "x: u16;");
    assert_ne!(changed, ECHO, "the fixture has a field to change");
    fs::write(&cli.echo, changed).expect("write");
    let (ok, text) = run_lock(&[]);
    assert!(!ok, "a changed fingerprint at major 1 must fail: {text}");
    assert!(text.contains("recorded fingerprint changed"), "{text}");
    let (ok, text) = run_lock(&["--update"]);
    assert!(
        !ok,
        "--update must refuse a change at a stable major: {text}"
    );

    // Bad usage and a bad definition fail without a panic.
    let (ok, text) = cli.run(&["frobnicate"]);
    assert!(!ok, "{text}");
    assert!(text.contains("unknown command"), "{text}");
    let (ok, text) = cli.run(&["check"]);
    assert!(!ok, "{text}");
    assert!(text.contains("no definition files"), "{text}");
    let bad = cli.dir.join("bad.oidl");
    fs::write(&bad, "library").expect("write");
    let (ok, text) = cli.run(&["check", &bad.display().to_string()]);
    assert!(!ok, "{text}");
    assert!(
        text.contains("bad.oidl:1:"),
        "the error has a position: {text}"
    );
}
