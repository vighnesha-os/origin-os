//! Host tests of the names gate, on sources made by hand.

use super::*;

fn files(entries: &[(&str, &str)]) -> BTreeMap<String, String> {
    entries
        .iter()
        .map(|(path, text)| ((*path).to_string(), (*text).to_string()))
        .collect()
}

#[test]
fn the_proof_process_constructors_are_found_as_paths_and_definitions() {
    for text in [
        "let p = Process::proof(LIMITS, (CODE, CODE + PAGE), 0)?;",
        "let c = Process::proof_child_of(creator, LIMITS, range)?;",
        "Self::proof(limits, origin, creator.principal)",
        "pub(crate) fn proof(limits: Limits) -> Option<Self> { None }",
        "Process :: proof (a, b, c)",
    ] {
        assert!(names_proof_process(&lex::tokens(text)), "{text}");
    }
    for text in [
        "// Process::proof is for proofs",
        "let s = \"Process::proof\";",
        "let proof = Process::pinned_child_of(creator, limits);",
        "fn prove_proof() {}",
        "let p = proof(x);",
    ] {
        assert!(!names_proof_process(&lex::tokens(text)), "{text}");
    }
}

#[test]
fn the_call_numbers_are_found_as_a_path_and_in_a_group() {
    for text in [
        "use origin_abi::syscall as sys;",
        "let n = origin_abi::syscall::DEBUG_REPORT;",
        "use origin_abi::{Error, syscall};",
        "use origin_abi::{start::{self, role}, syscall as numbers};",
    ] {
        assert!(names_call_numbers(&lex::tokens(text)), "{text}");
    }
    for text in [
        "use origin_abi::start::role;",
        "use origin_abi::{Error, start};",
        "// origin_abi::syscall holds the numbers",
        "use origin_kernel_client::origin_kernel::kernel::client; client::thread_exit(0);",
    ] {
        assert!(!names_call_numbers(&lex::tokens(text)), "{text}");
    }
}

#[test]
fn a_name_off_the_list_and_a_stale_list_entry_fail() {
    let listed = [("a.rs", "a reason")];
    // As listed.
    let good = files(&[("a.rs", "Process::proof(x)"), ("b.rs", "fn other() {}")]);
    assert!(problems("it", &good, &listed, names_proof_process).is_empty());
    // A new file names it.
    let more = files(&[
        ("a.rs", "Process::proof(x)"),
        ("b.rs", "Process::proof_child_of(c, l, r)"),
    ]);
    let found = problems("it", &more, &listed, names_proof_process);
    assert!(
        found.len() == 1 && found[0].contains("b.rs names it and is not on the list"),
        "{found:#?}"
    );
    // The listed file no longer does, and a listed file that is gone.
    let stale = files(&[("a.rs", "fn other() {}")]);
    let found = problems("it", &stale, &listed, names_proof_process);
    assert!(
        found.len() == 1 && found[0].contains("no longer names it"),
        "{found:#?}"
    );
    let gone = files(&[("b.rs", "fn other() {}")]);
    let found = problems("it", &gone, &listed, names_proof_process);
    assert!(
        found.len() == 1 && found[0].contains("is not in the tree"),
        "{found:#?}"
    );
}

#[test]
fn the_tree_passes() {
    assert_eq!(gate(), Ok(()));
}

#[test]
fn the_raw_write_is_found_as_a_call_and_a_definition() {
    for text in [
        "self.memory.write_frame(self.talk, GO, &GO_ON.to_le_bytes())",
        "process.memory()?.write_frame(frame, 0, bytes)",
        "space.write_frame (frame, 0, bytes)",
        "pub fn write_frame(&self, frame: UserFrame) -> bool { true }",
    ] {
        assert!(names_raw_write(&lex::tokens(text)), "{text}");
    }
    for text in [
        "process.write_page(frame, 0, bytes)",
        "// UserSpace::write_frame does not refuse it",
        "let s = \"write_frame(\";",
        "let write_frame_count = 0;",
    ] {
        assert!(!names_raw_write(&lex::tokens(text)), "{text}");
    }
}
