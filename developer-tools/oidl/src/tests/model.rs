//! Pins the independent model of the wire format against hand-computed messages: the corpus is
//! labelled by this model, so it must be right on its own terms.

use crate::model::{Compilation, Method};
use crate::sema::{Input, compile};
use crate::wire::{HandleInfo, Reason, check_message};

const KERNEL: &str = include_str!("../../../../sdk/interfaces/origin.kernel.oidl");
const ECHO: &str = include_str!("../../tests/fixtures/echo.oidl");
const CALLS: &str = include_str!("../../tests/fixtures/calls.oidl");
/// The real versions file, which the kernel's definition names its version from (§9).
const VERSIONS: &str = include_str!("../../../../origin-version.toml");

fn compilation() -> Compilation {
    let inputs = [("k", KERNEL), ("e", ECHO), ("c", CALLS)]
        .iter()
        .map(|(file, text)| Input {
            file: (*file).to_string(),
            text: text.as_bytes().to_vec(),
        })
        .collect::<Vec<_>>();
    compile(&inputs, Some(VERSIONS)).expect("valid")
}

fn method<'c>(comp: &'c Compilation, name: &str) -> &'c Method {
    comp.libraries
        .iter()
        .find(|l| l.name == "origin.fixture.echo")
        .expect("echo")
        .interfaces[0]
        .methods
        .iter()
        .find(|m| m.name == name)
        .expect("method")
}

/// docs/spec/OIDL.md, the worked example of §7.9.
const PUT: [u8; 40] = [
    0x28, 0, 0, 0, 1, 1, 0, 0, 5, 0, 0, 0, 7, 0, 0, 0, // header
    1, 0, 2, 1, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, // inline
    b'h', b'i', 0, 0, 0, 0, 0, 0, // the name
];
const HANDLE: HandleInfo = HandleInfo {
    object: 4,
    rights: 1,
};

fn verdict(method: &Method, bytes: &[u8], handles: &[HandleInfo]) -> Result<(), Reason> {
    check_message(method, "origin.fixture.echo", true, 2, bytes, handles)
}

#[test]
fn the_worked_example_is_accepted() {
    let comp = compilation();
    assert_eq!(verdict(method(&comp, "put"), &PUT, &[HANDLE]), Ok(()));
}

#[test]
fn each_violation_gets_its_own_reason_in_the_specified_order() {
    let comp = compilation();
    let put = method(&comp, "put");
    let with = |edit: &dyn Fn(&mut Vec<u8>)| {
        let mut bytes = PUT.to_vec();
        edit(&mut bytes);
        bytes
    };
    let cases: Vec<(&str, Vec<u8>, Vec<HandleInfo>, Reason)> = vec![
        (
            "shorter than a header",
            PUT[..15].to_vec(),
            vec![HANDLE],
            Reason::Size,
        ),
        (
            "size disagrees",
            with(&|b| b[0] = 0x20),
            vec![HANDLE],
            Reason::Size,
        ),
        (
            "not a multiple of 8",
            {
                let mut b = PUT.to_vec();
                b.push(0);
                b[0] = 41;
                b
            },
            vec![HANDLE],
            Reason::Size,
        ),
        (
            "wire revision",
            with(&|b| b[4] = 2),
            vec![HANDLE],
            Reason::Wire,
        ),
        (
            "unknown kind",
            with(&|b| b[5] = 9),
            vec![HANDLE],
            Reason::Kind,
        ),
        (
            "not a request",
            with(&|b| b[5] = 2),
            vec![HANDLE],
            Reason::Direction,
        ),
        (
            "reserved",
            with(&|b| b[7] = 1),
            vec![HANDLE],
            Reason::Reserved,
        ),
        (
            "no txid on a two-way call",
            with(&|b| b[12] = 0),
            vec![HANDLE],
            Reason::Txid,
        ),
        (
            "another method",
            with(&|b| b[8] = 6),
            vec![HANDLE],
            Reason::Ordinal,
        ),
        (
            "a bool of 2",
            with(&|b| b[16] = 2),
            vec![HANDLE],
            Reason::Bool,
        ),
        (
            "padding",
            with(&|b| b[17] = 1),
            vec![HANDLE],
            Reason::Padding,
        ),
        (
            "count over the bound",
            with(&|b| b[20] = 17),
            vec![HANDLE],
            Reason::Count,
        ),
        (
            "slot is not the next index",
            with(&|b| b[24] = 1),
            vec![HANDLE],
            Reason::HandleSlot,
        ),
        ("no handle", PUT.to_vec(), vec![], Reason::HandleCount),
        (
            "a handle too many",
            PUT.to_vec(),
            vec![HANDLE, HANDLE],
            Reason::HandleCount,
        ),
        (
            "wrong object",
            PUT.to_vec(),
            vec![HandleInfo {
                object: 5,
                rights: 1,
            }],
            Reason::HandleType,
        ),
        (
            "a right missing",
            PUT.to_vec(),
            vec![HandleInfo {
                object: 4,
                rights: 0,
            }],
            Reason::HandleRights,
        ),
        (
            "padding at the end of the inline part",
            with(&|b| b[31] = 1),
            vec![HANDLE],
            Reason::Padding,
        ),
        (
            "a longer body",
            {
                let mut b = PUT.to_vec();
                b.extend_from_slice(&[0; 8]);
                b[0] = 48;
                b
            },
            vec![HANDLE],
            Reason::Size,
        ),
        (
            "not UTF-8",
            with(&|b| b[32] = 0xFF),
            vec![HANDLE],
            Reason::Utf8,
        ),
        ("a NUL", with(&|b| b[33] = 0), vec![HANDLE], Reason::Nul),
        (
            "padding after the name",
            with(&|b| b[39] = 1),
            vec![HANDLE],
            Reason::Padding,
        ),
    ];
    for (name, bytes, handles, reason) in cases {
        assert_eq!(verdict(put, &bytes, &handles), Err(reason), "{name}");
    }
}

#[test]
fn the_corpus_is_bounded_on_a_huge_message() {
    // A message near the limit must not make the corpus unbounded (§14).
    let definition = "/// lib\nlibrary test.big;\ntransport channel;\nversion 1.65535;\nuse origin.kernel;\n/// i\ninterface i {\n/// m\nmethod m = 1 (a: array<u8, 8192>) errors() requires none;\n}\n";
    let inputs = vec![
        Input {
            file: "k".to_string(),
            text: KERNEL.as_bytes().to_vec(),
        },
        Input {
            file: "b".to_string(),
            text: definition.as_bytes().to_vec(),
        },
    ];
    let comp = compile(&inputs, Some(VERSIONS)).expect("valid");
    let library = comp
        .libraries
        .iter()
        .find(|l| l.name == "test.big")
        .expect("big");
    let text = crate::corpus::corpus_for(library);
    assert!(text.len() <= crate::corpus::MAX_CORPUS_BYTES);
    assert!(text.starts_with("# oidl-corpus 1 test.big"));
    assert!(text.lines().count() > 100, "it still has cases");
}

#[test]
fn a_one_way_message_has_no_transaction_id_and_a_response_has_one() {
    let comp = compilation();
    let note = method(&comp, "note");
    let mut body = vec![
        0x18, 0, 0, 0, 1, 1, 0, 0, 8, 0, 0, 0, 0, 0, 0, 0, // header
        0, 0, 0, 0, 0, 0, 0, 0, // inline: count 0, padding
    ];
    assert_eq!(
        check_message(note, "origin.fixture.echo", true, 2, &body, &[]),
        Ok(())
    );
    body[12] = 3;
    assert_eq!(
        check_message(note, "origin.fixture.echo", true, 2, &body, &[]),
        Err(Reason::Txid)
    );

    let ping = method(&comp, "ping");
    let mut response = vec![
        0x18, 0, 0, 0, 1, 2, 0, 0, 9, 0, 0, 0, 3, 0, 0, 0, // header, a response
        5, 0, 0, 0, 0, 0, 0, 0, // value
    ];
    assert_eq!(
        check_message(ping, "origin.fixture.echo", false, 2, &response, &[]),
        Ok(())
    );
    response[12] = 0;
    assert_eq!(
        check_message(ping, "origin.fixture.echo", false, 2, &response, &[]),
        Err(Reason::Txid)
    );
}
