//! Mutation soaks (CLAUDE.md §72): seeded, deterministic mutations of known-good definitions,
//! at byte and at token level. A failure is reproduced from its seed and case number alone.
//! Nothing may panic, and whatever is accepted must keep the specification's invariants.

use crate::layout;
use crate::lex::{self, Punct, Token};
use crate::sema::{Input, compile};
use crate::testing::{Rng, cases};
use crate::{codegen, corpus, fingerprint, wire};

const KERNEL: &str = include_str!("../../../../sdk/interfaces/origin.kernel.oidl");
const ECHO: &str = include_str!("../../tests/fixtures/echo.oidl");
const CALLS: &str = include_str!("../../tests/fixtures/calls.oidl");
/// The real versions file, which the kernel's definition names its version from (§9).
const VERSIONS: &str = include_str!("../../../../origin-version.toml");

fn render(tokens: &[lex::Spanned]) -> String {
    let mut out = String::new();
    for spanned in tokens {
        match &spanned.token {
            Token::Ident(text) => out.push_str(text),
            Token::Number(value) => out.push_str(&value.to_string()),
            Token::Str(text) => {
                out.push('"');
                out.push_str(text);
                out.push('"');
            }
            Token::Doc(text) => {
                out.push_str("/// ");
                out.push_str(text);
                out.push('\n');
                continue;
            }
            Token::Punct(p) => out.push_str(match p {
                Punct::LBrace => "{",
                Punct::RBrace => "}",
                Punct::LParen => "(",
                Punct::RParen => ")",
                Punct::Lt => "<",
                Punct::Gt => ">",
                Punct::Comma => ",",
                Punct::Semi => ";",
                Punct::Colon => ":",
                Punct::Eq => "=",
                Punct::Dot => ".",
                Punct::At => "@",
                Punct::Pipe => "|",
                Punct::Minus => "-",
                Punct::Arrow => "->",
            }),
            Token::Eof => {}
        }
        out.push(' ');
    }
    out
}

/// Replaces or removes tokens, keeping the file lexically plausible so that later stages are
/// reached far more often than by byte mutation alone.
fn mutate_tokens(rng: &mut Rng, text: &str) -> Vec<u8> {
    let Ok(mut tokens) = lex::lex("m", text.as_bytes()) else {
        return text.as_bytes().to_vec();
    };
    let edits = 1 + rng.below(3);
    for _ in 0..edits {
        let len = tokens.len().saturating_sub(1).max(1);
        let at = rng.below(len);
        match rng.below(6) {
            0 => {
                tokens.remove(at);
            }
            1 => {
                let copy = tokens[at].clone();
                tokens.insert(at, copy);
            }
            2 => {
                let other = rng.below(len);
                tokens.swap(at, other);
            }
            3 => tokens[at].token = Token::Number(rng.edge_u64()),
            4 => tokens[at].token = Token::Ident(random_word(rng)),
            _ => {
                tokens[at].token = match rng.below(5) {
                    0 => Token::Punct(Punct::Comma),
                    1 => Token::Punct(Punct::Semi),
                    2 => Token::Punct(Punct::RBrace),
                    3 => Token::Punct(Punct::Pipe),
                    _ => Token::Str("x".to_string()),
                };
            }
        }
    }
    render(&tokens).into_bytes()
}

fn random_word(rng: &mut Rng) -> String {
    const WORDS: &[&str] = &[
        "u8",
        "u16",
        "u32",
        "u64",
        "i8",
        "i64",
        "bool",
        "handle",
        "optional",
        "array",
        "vector",
        "string",
        "any",
        "none",
        "status",
        "rights",
        "ok",
        "fault",
        "channel",
        "sized",
        "struct",
        "enum",
        "bits",
        "const",
        "method",
        "oneway",
        "errors",
        "requires",
        "never",
        "else",
        "x",
        "y",
        "read_sizes",
        "invalid_args",
        "bad_handle",
        "max_message_bytes",
        "kernel",
        "echo",
    ];
    rng.pick(WORDS).to_string()
}

fn mutate_bytes(rng: &mut Rng, text: &str) -> Vec<u8> {
    let mut bytes = text.as_bytes().to_vec();
    let edits = 1 + rng.below(4);
    for _ in 0..edits {
        if bytes.is_empty() {
            break;
        }
        let at = rng.below(bytes.len());
        match rng.below(5) {
            0 => bytes[at] = rng.byte(),
            1 => {
                bytes.remove(at);
            }
            2 => bytes.insert(at, rng.byte()),
            3 => bytes[at] ^= 1 << rng.below(8),
            _ => {
                // A run of digits, where a number's limits are tested.
                bytes.insert(at, b'9');
                bytes.insert(at, b'9');
            }
        }
    }
    bytes
}

/// What every accepted definition must satisfy. The generators are run on a sample: building
/// a whole corpus for each of tens of thousands of definitions would take minutes.
fn check_accepted(comp: &crate::model::Compilation, case: usize) {
    let sampled = case.is_multiple_of(32);
    if sampled {
        let files = codegen::generate(comp);
        assert!(!files.is_empty());
    }
    for library in &comp.libraries {
        let _ = fingerprint::fingerprint_file(library);
        if sampled {
            let corpus_text = corpus::corpus_for(library);
            assert!(corpus_text.starts_with("# oidl-corpus 1 "));
        }
        for interface in &library.interfaces {
            for method in &interface.methods {
                for params in [&method.params, &method.returns] {
                    for minor in method.minors(library.version.minor) {
                        let layout = layout::message_layout(params, minor);
                        assert_eq!(layout.inline_size & 7, 0);
                        assert!(
                            layout.inline_size
                                <= layout::max_message_size(params, library.version.minor)
                        );
                    }
                }
                // A method's validator verdict never panics on any register set.
                let _ = wire::check_call(method, true, [u64::MAX; 6]);
            }
        }
    }
}

fn soak(seed: u64, count: usize, mutate: fn(&mut Rng, &str) -> Vec<u8>, what: &str) {
    const SOURCES: [(&str, &str); 3] = [("kernel", KERNEL), ("echo", ECHO), ("calls", CALLS)];
    let mut accepted = 0usize;
    let mut refused = 0usize;
    for case in 0..count {
        let mut rng = Rng::for_case(seed, case);
        let which = rng.below(SOURCES.len());
        let (name, text) = SOURCES[which];
        let mutated = mutate(&mut rng, text);
        let mut inputs: Vec<Input> = SOURCES
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != which)
            .map(|(_, (n, t))| Input {
                file: (*n).to_string(),
                text: t.as_bytes().to_vec(),
            })
            .collect();
        inputs.push(Input {
            file: name.to_string(),
            text: mutated,
        });
        match compile(&inputs, Some(VERSIONS)) {
            Ok(comp) => {
                accepted += 1;
                check_accepted(&comp, case);
            }
            Err(diagnostic) => {
                refused += 1;
                assert!(diagnostic.line >= 1, "{what} case {case}: {diagnostic}");
            }
        }
    }
    println!("{what}: {count} cases, {accepted} accepted, {refused} refused");
    assert!(refused > 0, "{what}: the mutations never changed anything");
}

#[test]
fn mutated_bytes_never_panic_and_what_passes_keeps_the_invariants() {
    soak(0x4F49_444C_0001, cases(20_000), mutate_bytes, "byte soak");
}

#[test]
fn mutated_tokens_never_panic_and_what_passes_keeps_the_invariants() {
    soak(0x4F49_444C_0002, cases(20_000), mutate_tokens, "token soak");
}

#[test]
fn the_same_seed_gives_the_same_mutants() {
    let mut a = Rng::for_case(7, 11);
    let mut b = Rng::for_case(7, 11);
    assert_eq!(mutate_bytes(&mut a, ECHO), mutate_bytes(&mut b, ECHO));
}

/// A definition that survives token mutation and is then refused is the point, but a soak that
/// accepted nothing would not reach the generators. This pins that they are reached.
#[test]
fn the_token_soak_reaches_the_generators() {
    let mut accepted = 0usize;
    for case in 0..2000 {
        let mut rng = Rng::for_case(0x4F49_444C_0002, case);
        let mutated = mutate_tokens(&mut rng, ECHO);
        let inputs = vec![
            Input {
                file: "kernel".to_string(),
                text: KERNEL.as_bytes().to_vec(),
            },
            Input {
                file: "calls".to_string(),
                text: CALLS.as_bytes().to_vec(),
            },
            Input {
                file: "echo".to_string(),
                text: mutated,
            },
        ];
        if compile(&inputs, Some(VERSIONS)).is_ok() {
            accepted += 1;
        }
    }
    assert!(
        accepted > 0,
        "token mutation never produced a valid definition"
    );
}
