//! The fingerprint and compatibility gate (`docs/spec/OIDL.md` §13).

use crate::fingerprint::{self, LockFinding, check_lock, fingerprints, render_lock};
use crate::model::Version;
use crate::sema::{Input, compile};

const ROLES: &str = "/// r\nlibrary test.kernel;\ntransport syscall;\nversion 1.0;\n/// s\nenum status: i64 { ok = 0; invalid_args = -1; access_denied = -4; fault = -6; }\n/// b\nconst max_message_bytes: u32 = 65536;\n/// h\nconst max_message_handles: u32 = 64;\n";

fn compiled(body: &str) -> crate::model::Compilation {
    let text = format!(
        "/// lib\nlibrary test.sys;\ntransport channel;\nversion 1.3;\nuse test.kernel;\n{body}\n"
    );
    compile(
        &[
            Input {
                file: "roles".to_string(),
                text: ROLES.as_bytes().to_vec(),
            },
            Input {
                file: "c".to_string(),
                text: text.into_bytes(),
            },
        ],
        None,
    )
    .expect("valid")
}

fn prints(body: &str) -> Vec<[u8; 32]> {
    let comp = compiled(body);
    let library = comp
        .libraries
        .iter()
        .find(|l| l.name == "test.sys")
        .expect("library");
    fingerprints(library)
        .into_iter()
        .map(|p| p.stable)
        .collect()
}

const BASE: &str = "/// e\nenum e: u8 { a = 1; b = 2; }\n/// i\ninterface i {\n/// m\nmethod m = 1 (x: u32, y: bool) -> (z: e) errors(invalid_args) requires none;\n}\n";

#[test]
fn comments_documentation_and_declaration_order_are_not_abi() {
    let base = prints(BASE);
    assert_eq!(
        base,
        prints(
            "// a comment\n/// e\nenum e: u8 { a = 1; b = 2; }\n/// other words\n/// i\ninterface i {\n/// Different documentation.\n/// On two lines.\nmethod m = 1 (x: u32, y: bool) -> (z: e) errors(invalid_args) requires none;\n}\n"
        )
    );
    assert_eq!(
        base,
        prints(
            "/// i\ninterface i {\n/// m\nmethod m = 1 (x: u32, y: bool) -> (z: e) errors(invalid_args) requires none;\n}\n/// e\nenum e: u8 { b = 2; a = 1; }\n"
        ),
        "enum member order is not ABI, values are"
    );
}

#[test]
fn every_abi_change_changes_the_fingerprint() {
    let base = prints(BASE);
    let changed = [
        // A value, a name and an ordinal.
        "/// e\nenum e: u8 { a = 1; b = 3; }\n/// i\ninterface i {\n/// m\nmethod m = 1 (x: u32, y: bool) -> (z: e) errors(invalid_args) requires none;\n}\n",
        "/// e\nenum e: u8 { a = 1; c = 2; }\n/// i\ninterface i {\n/// m\nmethod m = 1 (x: u32, y: bool) -> (z: e) errors(invalid_args) requires none;\n}\n",
        "/// e\nenum e: u8 { a = 1; b = 2; }\n/// i\ninterface i {\n/// m\nmethod m = 2 (x: u32, y: bool) -> (z: e) errors(invalid_args) requires none;\n}\n",
        // A parameter's order and type.
        "/// e\nenum e: u8 { a = 1; b = 2; }\n/// i\ninterface i {\n/// m\nmethod m = 1 (y: bool, x: u32) -> (z: e) errors(invalid_args) requires none;\n}\n",
        "/// e\nenum e: u8 { a = 1; b = 2; }\n/// i\ninterface i {\n/// m\nmethod m = 1 (x: u64, y: bool) -> (z: e) errors(invalid_args) requires none;\n}\n",
        // The errors and the requirement.
        "/// e\nenum e: u8 { a = 1; b = 2; }\n/// i\ninterface i {\n/// m\nmethod m = 1 (x: u32, y: bool) -> (z: e) errors() requires none;\n}\n",
        "/// e\nenum e: u8 { a = 1; b = 2; }\n/// c\ncapability a.b;\n/// i\ninterface i {\n/// m\nmethod m = 1 (x: u32, y: bool) -> (z: e) errors(invalid_args) requires a.b;\n}\n",
    ];
    for (index, body) in changed.iter().enumerate() {
        assert_ne!(
            base[0],
            prints(body)[0],
            "change {index} left the fingerprint alone"
        );
    }
}

#[test]
fn an_addition_leaves_every_earlier_minor_alone() {
    let base = prints(BASE);
    assert_eq!(base.len(), 4, "minors 0 to 3");
    let added = prints(
        "/// e\nenum e: u8 { a = 1; b = 2; @since(1.2) c = 3; }\n/// i\ninterface i {\n/// m\nmethod m = 1 (x: u32, y: bool) -> (z: e) errors(invalid_args) requires none;\n}\n",
    );
    assert_eq!(base[0], added[0]);
    assert_eq!(base[1], added[1]);
    assert_ne!(base[2], added[2], "minor 2 has the new member");
    assert_ne!(base[3], added[3]);
    let new_param = prints(
        "/// e\nenum e: u8 { a = 1; b = 2; }\n/// i\ninterface i {\n/// m\nmethod m = 1 (x: u32, y: bool, @since(1.3) w: u8) -> (z: e) errors(invalid_args) requires none;\n}\n",
    );
    assert_eq!(base[..3], new_param[..3]);
    assert_ne!(base[3], new_param[3]);
    let new_method = prints(
        "/// e\nenum e: u8 { a = 1; b = 2; }\n/// i\ninterface i {\n/// m\nmethod m = 1 (x: u32, y: bool) -> (z: e) errors(invalid_args) requires none;\n/// n\n@since(1.1) method n = 2 () errors() requires none;\n}\n",
    );
    assert_eq!(base[0], new_method[0]);
    assert_ne!(base[1], new_method[1]);
}

#[test]
fn a_new_item_with_since_leaves_every_earlier_minor_alone() {
    let base = prints(BASE);
    for (name, body) in [
        ("a const", "/// c\n@since(1.2) const x: u32 = 1;\n"),
        ("a struct", "/// s\n@since(1.2) struct t { x: u8; }\n"),
        ("an enum", "/// t\n@since(1.2) enum t: u8 { y = 1; }\n"),
        ("bits", "/// t\n@since(1.2) bits t: u8 { y = 1; }\n"),
        ("a capability", "/// k\n@since(1.2) capability k.m;\n"),
    ] {
        let added = prints(&format!("{BASE}{body}"));
        assert_eq!(base[0], added[0], "{name} changed minor 0");
        assert_eq!(base[1], added[1], "{name} changed minor 1");
        assert_ne!(base[2], added[2], "{name} is not in minor 2");
    }
    // Without @since the new item is part of every minor, so the earlier ones change.
    assert_ne!(
        base[0],
        prints(&format!("{BASE}/// x\nconst x: u32 = 1;\n"))[0]
    );
}

#[test]
fn reordering_interfaces_is_not_abi() {
    let two = |first: &str, second: &str| {
        prints(&format!(
            "/// {first}\ninterface {first} {{\n/// a\nmethod a = 1 () errors() requires none;\n}}\n/// {second}\ninterface {second} {{\n/// a\nmethod a = 1 () errors() requires none;\n}}\n"
        ))
    };
    assert_eq!(two("a", "b"), two("b", "a"));
}

fn sized_comp(fields: &str) -> crate::model::Compilation {
    let text = format!(
        "/// lib\nlibrary test.sized;\ntransport syscall;\nversion 1.3;\n/// s\nenum status: i64 {{ ok = 0; invalid_args = -1; fault = -6; }}\n/// t\nsized struct t {{ {fields} }}\n"
    );
    compile(
        &[Input {
            file: "c".to_string(),
            text: text.into_bytes(),
        }],
        None,
    )
    .expect("valid")
}

#[test]
fn a_later_field_of_a_sized_struct_leaves_earlier_minors_alone() {
    let print = |fields: &str| {
        let comp = sized_comp(fields);
        fingerprints(&comp.libraries[0])
            .into_iter()
            .map(|p| p.stable)
            .collect::<Vec<_>>()
    };
    let base = print("a: u16;");
    let added = print("a: u16; @since(1.2) b: u64;");
    assert_eq!(base[0], added[0]);
    assert_eq!(
        base[1], added[1],
        "size and alignment at minor 1 are unchanged"
    );
    assert_ne!(base[2], added[2]);
}

/// A library whose method takes and returns its sized struct `t`.
fn sized_comp_used(fields: &str) -> crate::model::Compilation {
    let text = format!(
        "/// lib\nlibrary test.sized;\ntransport syscall;\nversion 1.3;\n/// s\nenum status: i64 {{ ok = 0; invalid_args = -1; fault = -6; }}\n/// t\nsized struct t {{ {fields} }}\n/// i\ninterface i {{\n/// m\nmethod m = 1 (p: t) -> (r: t) errors(invalid_args, fault) requires none;\n}}\n"
    );
    compile(
        &[Input {
            file: "c".to_string(),
            text: text.into_bytes(),
        }],
        None,
    )
    .expect("valid")
}

#[test]
fn a_later_field_of_a_sized_struct_a_method_uses_leaves_earlier_minors_alone() {
    // The method names the struct, and that reference must not carry a size from a later
    // minor: growing a sized struct is an addition (§9), which changes no earlier minor (§13).
    let print = |fields: &str| {
        let comp = sized_comp_used(fields);
        fingerprints(&comp.libraries[0])
            .into_iter()
            .map(|p| p.stable)
            .collect::<Vec<_>>()
    };
    let base = print("a: u16;");
    let added = print("a: u16; @since(1.2) b: u64;");
    assert_eq!(base[..2], added[..2], "minors 0 and 1 are unchanged");
    assert_ne!(base[2], added[2], "minor 2 has the new field");
    assert_ne!(base[3], added[3]);
    // Each minor names the struct with its size at that minor.
    let comp = sized_comp_used("a: u16; @since(1.2) b: u64;");
    let library = &comp.libraries[0];
    let t = library.structs.iter().find(|s| s.name == "t").expect("t");
    for minor in 0..=3 {
        let text = fingerprint::describe(library, minor, false);
        let named = format!("struct(t,{})", t.size_at(minor));
        assert!(
            text.contains(&format!(" in p {named} ")),
            "minor {minor}: {text}"
        );
        assert!(
            text.contains(&format!(" out r {named} ")),
            "minor {minor}: {text}"
        );
    }
    assert_ne!(t.size_at(1), t.size_at(2), "the struct did grow");
}

#[test]
fn a_used_librarys_layout_is_part_of_this_librarys_fingerprint() {
    let build = |fields: &str, member_line: &str| {
        let kernel = format!(
            "/// k\nlibrary test.kernel;\ntransport syscall;\nversion 0.3;\n/// s\nenum status: i64 {{ ok = 0; invalid_args = -1; access_denied = -4; }}\n/// b\nconst max_message_bytes: u32 = 65536;\n/// h\nconst max_message_handles: u32 = 64;\n/// d\nstruct data {{ {fields} }}\n/// e\nenum e: u8 {{ {member_line} }}\n"
        );
        let service = "/// lib\nlibrary test.user;\ntransport channel;\nversion 1.0;\nuse test.kernel;\n/// i\ninterface i {\n/// m\nmethod m = 1 (d: kernel.data, c: kernel.e) errors() requires none;\n}\n";
        let comp = compile(
            &[
                Input {
                    file: "k".to_string(),
                    text: kernel.into_bytes(),
                },
                Input {
                    file: "u".to_string(),
                    text: service.as_bytes().to_vec(),
                },
            ],
            None,
        )
        .expect("valid");
        let user = comp
            .libraries
            .iter()
            .find(|l| l.name == "test.user")
            .expect("user");
        fingerprints(user)[0].stable
    };
    let base = build("a: u16; b: u16;", "x = 1; y = 2;");
    assert_ne!(
        base,
        build("b: u16; a: u16;", "x = 1; y = 2;"),
        "a used struct's layout"
    );
    assert_ne!(
        base,
        build("a: u16; b: u32;", "x = 1; y = 2;"),
        "a used struct's size"
    );
    assert_ne!(
        base,
        build("a: u16; b: u16;", "x = 5; y = 2;"),
        "a used enum's values"
    );
    assert_eq!(
        base,
        build("a: u16; b: u16;", "x = 1; y = 2; @since(0.3) z = 3;"),
        "a member added later is that library's own business"
    );
}

#[test]
fn call_numbers_are_outside_the_stable_fingerprint_but_inside_the_release_one() {
    let lib = |n: u32| {
        let text = format!(
            "/// lib\nlibrary test.c;\ntransport syscall;\nversion 1.0;\n/// s\nenum status: i64 {{ ok = 0; fault = -6; }}\n/// i\ninterface i {{\n/// m\nmethod m = {n} (x: u64) errors() requires none;\n}}\n"
        );
        compile(
            &[Input {
                file: "c".to_string(),
                text: text.into_bytes(),
            }],
            None,
        )
        .expect("valid")
    };
    let a = lib(1);
    let b = lib(2);
    let prints_a = fingerprints(&a.libraries[0]);
    let prints_b = fingerprints(&b.libraries[0]);
    assert_eq!(
        prints_a[0].stable, prints_b[0].stable,
        "§42: numbers are private"
    );
    assert_ne!(prints_a[0].release, prints_b[0].release);
    // Channel ordinals are ABI, and so are in the stable fingerprint: pinned above.
}

fn comp_at(version: &str, body: &str) -> crate::model::Compilation {
    let text = format!(
        "/// lib\nlibrary test.gate;\ntransport syscall;\nversion {version};\n/// s\nenum status: i64 {{ ok = 0; fault = -6; }}\n{body}\n"
    );
    compile(
        &[Input {
            file: "c".to_string(),
            text: text.into_bytes(),
        }],
        None,
    )
    .expect("valid")
}

const METHOD: &str =
    "/// i\ninterface i {\n/// m\nmethod m = 1 (x: u64) errors() requires none;\n}\n";
const OTHER: &str =
    "/// i\ninterface i {\n/// m\nmethod m = 1 (x: i64) errors() requires none;\n}\n";

#[test]
fn the_gate_passes_an_unchanged_lock_and_reports_a_new_minor() {
    let comp = comp_at("1.0", METHOD);
    let lock = render_lock(&comp);
    let report = check_lock(&comp, &lock);
    assert_eq!(report, fingerprint::LockReport::default());

    let newer = comp_at("1.1", METHOD);
    let report = check_lock(&newer, &lock);
    assert!(report.errors.is_empty());
    assert_eq!(
        report.unrecorded,
        vec![LockFinding::Unrecorded {
            library: "test.gate".to_string(),
            version: Version { major: 1, minor: 1 },
        }]
    );
}

#[test]
fn the_gate_refuses_a_change_at_a_stable_major_and_only_warns_at_major_zero() {
    let comp = comp_at("1.0", METHOD);
    let lock = render_lock(&comp);
    let changed = comp_at("1.0", OTHER);
    let report = check_lock(&changed, &lock);
    assert_eq!(
        report.errors,
        vec![LockFinding::Changed {
            library: "test.gate".to_string(),
            version: Version { major: 1, minor: 0 },
        }]
    );
    let zero = comp_at("0.1", METHOD);
    let zero_lock = render_lock(&zero);
    let zero_changed = comp_at("0.1", OTHER);
    let report = check_lock(&zero_changed, &zero_lock);
    assert!(report.errors.is_empty(), "major 0 promises nothing");
    assert_eq!(report.warnings.len(), 2, "minors 0 and 1 both changed");
}

#[test]
fn the_gate_refuses_a_major_that_went_backwards_even_at_major_zero() {
    let comp = comp_at("1.0", METHOD);
    let lock = render_lock(&comp);
    let downgraded = comp_at("0.0", OTHER);
    let report = check_lock(&downgraded, &lock);
    assert_eq!(
        report.errors,
        vec![LockFinding::Gone {
            library: "test.gate".to_string(),
            version: Version { major: 1, minor: 0 },
        }],
        "a change cannot be hidden by renumbering to major 0"
    );
}

#[test]
fn the_gate_refuses_a_recorded_library_that_is_not_among_the_inputs() {
    let comp = comp_at("1.0", METHOD);
    let mut lock = render_lock(&comp);
    lock.push_str(
        "other.lib 1.0 sha256:0000000000000000000000000000000000000000000000000000000000000000\n",
    );
    let report = check_lock(&comp, &lock);
    assert_eq!(
        report.errors,
        vec![LockFinding::Gone {
            library: "other.lib".to_string(),
            version: Version { major: 1, minor: 0 },
        }]
    );
}

#[test]
fn the_gate_refuses_a_malformed_lock_line() {
    let comp = comp_at("1.0", METHOD);
    let mut lock = render_lock(&comp);
    lock.push_str("not a lock line at all\n");
    let report = check_lock(&comp, &lock);
    assert_eq!(report.errors.len(), 1);
    assert!(matches!(report.errors[0], LockFinding::Malformed { .. }));
    let report = check_lock(&comp, "test.gate 1.0 sha256:shortdigest\n");
    assert_eq!(report.errors.len(), 1, "a digest of the wrong length");
    assert!(matches!(report.errors[0], LockFinding::Malformed { .. }));
}

#[test]
fn the_gate_refuses_a_recorded_minor_that_is_gone() {
    let comp = comp_at("1.2", METHOD);
    let lock = render_lock(&comp);
    let downgraded = comp_at("1.1", METHOD);
    let report = check_lock(&downgraded, &lock);
    assert_eq!(
        report.errors,
        vec![LockFinding::Gone {
            library: "test.gate".to_string(),
            version: Version { major: 1, minor: 2 },
        }]
    );
}

#[test]
fn a_very_high_minor_does_not_render_the_description_65536_times() {
    // The digests repeat, so nothing is rendered for a minor where nothing changes.
    let comp = comp_at("1.65535", METHOD);
    let prints = fingerprints(&comp.libraries[0]);
    assert_eq!(prints.len(), 65536);
    assert_eq!(prints[0].stable, prints[65535].stable);
}

#[test]
fn the_fingerprint_is_pinned() {
    // Changing the description's format is a compatibility event of its own (§13); pinning one
    // digest makes that a visible change here. Format 2, which records a handle's width
    // (decision item 237), moved it from 74a42979…505d, format 1's.
    let comp = comp_at("1.0", METHOD);
    assert_eq!(
        fingerprint::hex(&fingerprints(&comp.libraries[0])[0].stable),
        "b7bb3cbe175f3059d7e018f5d97ef1cdd80cf47b08f57072a97db745a8e0ebc2"
    );
}

#[test]
fn interface_ids_differ_by_name_and_are_stable() {
    let a = fingerprint::interface_id("origin.fixture.echo", "echo");
    let b = fingerprint::interface_id("origin.fixture.echo", "other");
    let c = fingerprint::interface_id("origin.fixture.calls", "echo");
    assert_ne!(a, b);
    assert_ne!(a, c);
    assert_eq!(a, fingerprint::interface_id("origin.fixture.echo", "echo"));
}

/// The shipped definitions, compiled against the real versions file.
fn shipped() -> crate::model::Compilation {
    let inputs = [
        (
            "origin.kernel.oidl",
            include_str!("../../../../sdk/interfaces/origin.kernel.oidl"),
        ),
        (
            "origin.directory.oidl",
            include_str!("../../../../sdk/interfaces/origin.directory.oidl"),
        ),
    ]
    .iter()
    .map(|(file, text)| Input {
        file: file.to_string(),
        text: text.as_bytes().to_vec(),
    })
    .collect::<Vec<_>>();
    compile(
        &inputs,
        Some(include_str!("../../../../origin-version.toml")),
    )
    .expect("valid")
}

/// Decision item 237: every library's description says, at every minor, how wide a handle is,
/// and the width it says is the one the generator emits, in Rust and in C.
#[test]
fn the_description_records_the_handle_width_the_generator_emits() {
    use crate::layout::HANDLE_BITS;
    let comp = shipped();
    for library in &comp.libraries {
        for minor in 0..=library.version.minor {
            for numbers in [false, true] {
                let text = fingerprint::describe(library, minor, numbers);
                let header: Vec<&str> = text.lines().take(5).collect();
                assert_eq!(header[0], "oidl-fingerprint 2");
                assert!(header[3].starts_with("transport "), "{text}");
                assert_eq!(header[4], format!("handle {HANDLE_BITS}"), "{text}");
            }
        }
    }
    // The runtime the generator writes holds a handle in exactly that width.
    let runtime = include_str!("../codegen/oidl_rt.rs.in");
    assert!(runtime.contains(&format!("pub struct Handle(u{HANDLE_BITS});")));
    assert!(runtime.contains(&format!(
        "impl Fixed for Handle {{\n    const SIZE: usize = {};",
        HANDLE_BITS / 8
    )));
    assert!(runtime.contains(&format!(
        "pub const fn from_abi(raw: u{HANDLE_BITS}) -> Self"
    )));
    // And the C header names it so.
    let files = crate::codegen::generate(&comp);
    let header = files
        .iter()
        .find(|f| f.path == "c/origin/kernel.h")
        .expect("the kernel's header");
    assert!(
        header
            .text
            .contains(&format!("typedef uint{HANDLE_BITS}_t origin_handle_t;"))
    );
}

/// The negative control of decision item 237: a handle of another width, planted where the
/// description is made, changes every minor's stable and release digest of every library, so a
/// change of a handle's width can never leave a lock as it was. (Before format 2 it left them all
/// as they were: S4-0's 32 to 64 bits showed only through two structures.)
#[test]
fn a_planted_handle_width_changes_every_digest() {
    let comp = shipped();
    for library in &comp.libraries {
        let real = fingerprints(library);
        let planted = fingerprint::fingerprints_with(library, 32);
        assert_eq!(real.len(), planted.len());
        assert!(!real.is_empty());
        for (a, b) in real.iter().zip(&planted) {
            assert_eq!(a.version, b.version);
            assert_ne!(a.stable, b.stable, "{} {}", library.name, a.version);
            assert_ne!(a.release, b.release, "{} {}", library.name, a.version);
        }
    }
}
