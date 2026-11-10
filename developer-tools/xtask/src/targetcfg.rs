//! CLAUDE.md §11's lint (decision item 123): what Origin code may not ask the compiler about its
//! target.
//!
//! Origin's userspace is compiled for the `x86_64-unknown-linux-gnu` triple, used only as a code
//! generator. On that triple `cfg(unix)`, `target_family = "unix"`, `target_env = "gnu"` and
//! `target_os = "linux"` are all true, so code that asks any of them takes a path written for
//! another operating system, silently. Refused, outside `developer-tools/` (host tools, which do
//! run on Linux and macOS):
//!
//! - `target_os = "linux"` in a `cfg` predicate;
//! - `unix` and `windows` as `cfg` predicates: they are the short spellings of `target_family`;
//! - `target_family` and `target_env`, anywhere in code or in a string, which also covers the
//!   names a build script reads them by (`CARGO_CFG_TARGET_FAMILY`, `CARGO_CFG_TARGET_ENV`), and
//!   `CARGO_CFG_UNIX` and `CARGO_CFG_WINDOWS` with them.
//!
//! A predicate is what `cfg(…)`, `cfg!(…)` and `cfg_attr(…, …)` test, and the arm heads of
//! `cfg_select!`. In a manifest it is what a `[target.'cfg(…)'.…]` table tests. `unix` as an
//! ordinary word (a variable holding Unix time, say) is nobody's concern, and neither is text in
//! a comment.
//!
//! `target_os = "none"` and `target_os = "uefi"` stay allowed: they are how the kernel's and the
//! loader's own code is told from its host tests.

use crate::lex::{self, Kind, Token};

/// One refusal in a file.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Hit {
    pub(crate) line: usize,
    pub(crate) what: String,
}

/// The names that are refused wherever they stand, in any letter case and inside longer names.
const REFUSED_NAMES: &[&str] = &["target_family", "target_env"];
/// What a build script reads to ask the questions `cfg(unix)` and `cfg(windows)` ask.
const REFUSED_VARIABLES: &[&str] = &["CARGO_CFG_UNIX", "CARGO_CFG_WINDOWS"];
/// The predicates that are short for a `target_family`.
const FAMILY_PREDICATES: &[&str] = &["unix", "windows"];

/// The refusals in one Rust source file.
pub(crate) fn rust_hits(source: &str) -> Vec<Hit> {
    let tokens = lex::tokens(source);
    let mut hits = Vec::new();
    let mut hit = |token: &Token<'_>, what: String| {
        hits.push(Hit {
            line: lex::line_of(source, token.start),
            what,
        });
    };
    for (i, token) in tokens.iter().enumerate() {
        // Names that are refused wherever they stand.
        if matches!(token.kind, Kind::Ident | Kind::Str { .. }) {
            let lower = token.text.to_ascii_lowercase();
            if let Some(name) = REFUSED_NAMES.iter().find(|name| lower.contains(**name)) {
                hit(token, format!("`{}` names `{name}`", token.text));
            } else if let Some(variable) = REFUSED_VARIABLES
                .iter()
                .find(|variable| token.text.contains(**variable))
            {
                let predicate = variable
                    .trim_start_matches("CARGO_CFG_")
                    .to_ascii_lowercase();
                hit(
                    token,
                    format!("`{variable}` asks what `cfg({predicate})` asks"),
                );
            }
        }
        // Predicates.
        if token.kind != Kind::Ident {
            continue;
        }
        let after_bang = usize::from(tokens.get(i + 1).is_some_and(|t| t.is_punct('!')));
        let open = i + 1 + after_bang;
        let Some(close) = tokens
            .get(open)
            .filter(|t| lex::bracket(t) == Some(1))
            .and_then(|_| lex::closing(&tokens, open))
        else {
            continue;
        };
        let body = tokens.get(open + 1..close).unwrap_or(&[]);
        let predicate: Vec<&Token<'_>> = match (token.text, after_bang) {
            ("cfg", _) => body.iter().collect(),
            // The condition is the first argument; the rest are the attributes it guards.
            ("cfg_attr", 0) => first_argument(body).iter().collect(),
            // Each arm is `predicate => { … }`: the heads are what lies outside every brace.
            ("cfg_select" | "cfg_match", 1) => outside_braces(body).collect(),
            _ => continue,
        };
        for (k, word) in predicate.iter().enumerate() {
            if FAMILY_PREDICATES.iter().any(|family| word.is_ident(family)) {
                hit(
                    word,
                    format!(
                        "`{}` as a `cfg` predicate: it is short for `target_family = \"{}\"`",
                        word.text, word.text
                    ),
                );
            }
            let equals_linux = predicate.get(k + 1).is_some_and(|t| t.is_punct('='))
                && predicate
                    .get(k + 2)
                    .is_some_and(|t| matches!(t.kind, Kind::Str { .. }) && t.text == "linux");
            if word.is_ident("target_os") && equals_linux {
                hit(
                    word,
                    "`target_os = \"linux\"` as a `cfg` predicate".to_string(),
                );
            }
        }
    }
    hits
}

/// The tokens of `body` before its first comma that is outside every bracket.
fn first_argument<'t, 'a>(body: &'t [Token<'a>]) -> &'t [Token<'a>] {
    let mut depth = 0;
    for (i, token) in body.iter().enumerate() {
        depth += lex::bracket(token).unwrap_or(0);
        if depth == 0 && token.is_punct(',') {
            return body.get(..i).unwrap_or(body);
        }
    }
    body
}

/// The tokens of `body` that are inside no pair of braces.
fn outside_braces<'t, 'a>(body: &'t [Token<'a>]) -> impl Iterator<Item = &'t Token<'a>> {
    let mut depth = 0u32;
    body.iter().filter(move |token| {
        if token.is_punct('{') {
            depth += 1;
            false
        } else if token.is_punct('}') {
            depth = depth.saturating_sub(1);
            false
        } else {
            depth == 0
        }
    })
}

/// The refusals in one Cargo manifest or configuration file: in the predicate of each `cfg(…)`
/// it names, as in `[target.'cfg(unix)'.dependencies]`. Its comments are not read.
pub(crate) fn manifest_hits(text: &str) -> Vec<Hit> {
    let mut hits = Vec::new();
    for (n, line) in text.lines().enumerate() {
        let mut rest = without_toml_comment(line);
        while let Some(at) = rest.find("cfg(") {
            let from = rest.get(at..).unwrap_or("");
            // Up to the parenthesis that closes the predicate, or the whole rest if none does.
            let mut depth = 0;
            let end = from
                .char_indices()
                .find_map(|(i, c)| {
                    depth += i32::from(c == '(') - i32::from(c == ')');
                    (depth == 0 && c == ')').then_some(i + 1)
                })
                .unwrap_or(from.len());
            // In a key quoted with `"`, the predicate's own quotes are escaped.
            let predicate = from.get(..end).unwrap_or(from).replace("\\\"", "\"");
            hits.extend(rust_hits(&predicate).into_iter().map(|found| Hit {
                line: n + 1,
                what: found.what,
            }));
            rest = from.get(end..).unwrap_or("");
        }
    }
    hits
}

/// A line of TOML up to its comment: a `#` outside every string starts one.
fn without_toml_comment(line: &str) -> &str {
    let mut quote = None;
    let mut escaped = false;
    for (i, c) in line.char_indices() {
        match (quote, c) {
            (Some('"'), '\\') if !escaped => {
                escaped = true;
                continue;
            }
            (Some(q), c) if c == q && !escaped => quote = None,
            (None, '"' | '\'') => quote = Some(c),
            (None, '#') => return line.get(..i).unwrap_or(line),
            _ => {}
        }
        escaped = false;
    }
    line
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repo_root;
    use std::fs;

    fn whats(source: &str) -> Vec<String> {
        rust_hits(source).into_iter().map(|h| h.what).collect()
    }

    fn assert_caught(source: &str, needle: &str) {
        let found = whats(source);
        assert!(
            found.iter().any(|w| w.contains(needle)),
            "expected {needle:?} for {source:?}, found {found:?}"
        );
    }

    fn assert_clean(source: &str) {
        let found = whats(source);
        assert!(found.is_empty(), "{source:?}: {found:?}");
    }

    #[test]
    fn linux_as_the_target_os_is_caught_as_before() {
        for source in [
            r#"#[cfg(target_os = "linux")] fn f() {}"#,
            r#"#[cfg(target_os="linux")] fn f() {}"#,
            "#[cfg(\n    target_os\n        = \"linux\"\n)]\nfn f() {}",
            r#"#[cfg(all(target_os = "linux", target_arch = "x86_64"))] fn f() {}"#,
            r#"#[cfg(not(target_os = "linux"))] fn f() {}"#,
            r#"#![cfg(target_os = "linux")]"#,
            r#"#[cfg_attr(target_os = "linux", path = "linux.rs")] mod sys;"#,
            r#"if cfg!(target_os = "linux") { }"#,
            r#"if cfg ! (target_os = r"linux") { }"#,
        ] {
            assert_caught(source, "`target_os = \"linux\"` as a `cfg` predicate");
        }
    }

    #[test]
    fn unix_and_windows_as_predicates_are_caught() {
        for source in [
            "#[cfg(unix)] fn f() {}",
            "#[cfg(not(unix))] fn f() {}",
            "#[cfg(any(unix, target_os = \"none\"))] fn f() {}",
            "#![cfg(unix)]",
            "#[cfg_attr(unix, path = \"posix.rs\")] mod sys;",
            "#[cfg_attr(all(test, unix), ignore)] fn f() {}",
            "let hosted = cfg!(unix);",
            "let hosted = cfg!( all( unix , test ) );",
            "cfg_select! { unix => { fn f() {} } _ => { fn f() {} } }",
            "cfg_select! { any(test, unix) => { } _ => { } }",
        ] {
            assert_caught(source, "`unix` as a `cfg` predicate");
        }
        for source in [
            "#[cfg(windows)] fn f() {}",
            "#[cfg(not(windows))] fn f() {}",
            "let w = cfg!(windows);",
        ] {
            assert_caught(source, "`windows` as a `cfg` predicate");
        }
    }

    #[test]
    fn target_family_and_target_env_are_caught_wherever_they_stand() {
        for (source, name) in [
            (
                r#"#[cfg(target_family = "unix")] fn f() {}"#,
                "target_family",
            ),
            (
                r#"#[cfg(not(target_family = "wasm"))] fn f() {}"#,
                "target_family",
            ),
            (r#"#[cfg(target_env = "gnu")] fn f() {}"#, "target_env"),
            (
                r#"#[cfg(any(target_env = "musl", test))] fn f() {}"#,
                "target_env",
            ),
            (r#"let gnu = cfg!(target_env = "gnu");"#, "target_env"),
            (
                r#"#[cfg_attr(target_env = "gnu", link(name = "c"))] extern "C" {}"#,
                "target_env",
            ),
            // A build script asks the same questions through the environment.
            (
                r#"let family = std::env::var("CARGO_CFG_TARGET_FAMILY");"#,
                "target_family",
            ),
            (
                r#"let env = std::env::var_os("CARGO_CFG_TARGET_ENV");"#,
                "target_env",
            ),
            (
                r#"println!("cargo::rustc-check-cfg=cfg(target_env, values(\"gnu\"))");"#,
                "target_env",
            ),
        ] {
            assert_caught(source, &format!("names `{name}`"));
        }
        assert_caught(
            r#"if std::env::var_os("CARGO_CFG_UNIX").is_some() {}"#,
            "`CARGO_CFG_UNIX` asks what `cfg(unix)` asks",
        );
        assert_caught(
            r#"let w = env::var("CARGO_CFG_WINDOWS");"#,
            "`CARGO_CFG_WINDOWS` asks what `cfg(windows)` asks",
        );
    }

    #[test]
    fn what_the_kernel_and_its_tests_really_write_passes() {
        assert_clean(
            r##"
            //! `cfg(unix)` and `target_env` in documentation are not code.
            // #[cfg(target_os = "linux")]
            /* #[cfg(target_family = "unix")] */
            #![cfg_attr(not(test), no_std)]
            #[cfg(target_os = "none")]
            mod kernel;
            #[cfg(target_os = "uefi")]
            mod loader;
            #[cfg(not(target_os = "none"))]
            mod host;
            #[cfg(all(test, not(loom)))]
            mod tests;
            #[cfg(target_arch = "x86_64")]
            #[cfg(target_pointer_width = "64")]
            #[cfg(feature = "unix")]
            #[cfg(feature = "test-trust-store")]
            #[cfg_attr(test, path = "unix.rs")]
            #[cfg_attr(not(any(test, target_os = "none")), allow(dead_code))]
            fn f() {
                // `unix` as an ordinary word: Unix time.
                let Some(unix) = when else { return };
                let base = unix.saturating_sub(at);
                let t = DateTime::from_unix_seconds(unix);
                let text = "unix seconds, on linux or windows";
                let os = "linux";
                self.windows.push(window);
                if cfg!(debug_assertions) && cfg!(feature = "developer-mode") {}
            }
            "##,
        );
        // A word after the condition of `cfg_attr` is an attribute, not a predicate.
        assert_clean("#[cfg_attr(test, derive(unix))] struct S;");
        // `target_os` compared with something else, or not compared at all.
        assert_clean(r#"#[cfg(target_os = "macos")] fn f() {}"#);
        assert_clean(r#"let target_os = "linux";"#);
        // An arm's body is not its head.
        assert_clean("cfg_select! { test => { let unix = 1; } _ => { let windows = 2; } }");
    }

    #[test]
    fn a_hit_says_which_line_it_is_on() {
        let source = "fn f() {}\n\n#[cfg(\n    unix\n)]\nfn g() {}\n";
        let hits = rust_hits(source);
        assert_eq!(hits.len(), 1, "{hits:?}");
        assert_eq!(hits[0].line, 4);
    }

    #[test]
    fn a_manifest_is_read_for_the_same_predicates() {
        for (text, needle) in [
            (
                "[target.'cfg(unix)'.dependencies]\nlibc = \"1\"\n",
                "`unix` as a `cfg` predicate",
            ),
            (
                "[target.'cfg(not(windows))'.dependencies]\n",
                "`windows` as a `cfg` predicate",
            ),
            (
                "[target.'cfg(target_os = \"linux\")'.dependencies]\n",
                "`target_os = \"linux\"` as a `cfg` predicate",
            ),
            (
                "[target.\"cfg(target_os = \\\"linux\\\")\".dependencies]\n",
                "`target_os = \"linux\"` as a `cfg` predicate",
            ),
            (
                "[target.'cfg(target_env = \"gnu\")'.dependencies]\n",
                "names `target_env`",
            ),
            (
                "[target.'cfg(target_family = \"unix\")'.build-dependencies]\n",
                "names `target_family`",
            ),
        ] {
            let hits = manifest_hits(text);
            assert!(
                hits.iter().any(|h| h.what.contains(needle) && h.line == 1),
                "{text:?}: {hits:?}"
            );
        }
        // The workspace's own manifest, and a comment that only speaks of the rule.
        let fine = "# `cfg(unix)` and target_env are refused (CLAUDE.md §11).\n[workspace.lints.rust]\nunexpected_cfgs = { level = \"warn\", check-cfg = ['cfg(loom)'] } # not cfg(unix)\n[target.x86_64-unknown-none]\nrustflags = [\"-C\", \"force-frame-pointers=yes\"]\n[target.'cfg(target_os = \"none\")'.dependencies]\n";
        assert!(manifest_hits(fine).is_empty(), "{:?}", manifest_hits(fine));
        // A `#` inside a string does not start a comment.
        let hidden = "description = \"#1\" # cfg(unix)\nkey = '#' # x\n[target.'cfg(unix)'.dependencies] # the real one\n";
        let hits = manifest_hits(hidden);
        assert_eq!(hits.len(), 1, "{hits:?}");
        assert_eq!(hits[0].line, 3);
    }

    /// Violations planted in the text of a real file: the kernel library's own conditions,
    /// turned into ones the userspace triple answers.
    #[test]
    fn a_real_file_changed_for_the_worse_is_caught() {
        let real =
            fs::read_to_string(repo_root().join("framekernel/src/lib.rs")).expect("the file");
        assert!(rust_hits(&real).is_empty(), "{:?}", rust_hits(&real));
        let condition = "#[cfg(target_os = \"none\")]";
        assert!(real.contains(condition));
        for (planted, needle) in [
            ("#[cfg(not(unix))]", "`unix` as a `cfg` predicate"),
            ("#[cfg(target_family = \"unix\")]", "names `target_family`"),
            ("#[cfg(not(target_env = \"gnu\"))]", "names `target_env`"),
            (
                "#[cfg(not(target_os = \"linux\"))]",
                "`target_os = \"linux\"`",
            ),
        ] {
            let changed = real.replacen(condition, planted, 1);
            let hits = rust_hits(&changed);
            assert_eq!(hits.len(), 1, "{planted}: {hits:?}");
            assert!(hits[0].what.contains(needle), "{planted}: {hits:?}");
        }
    }
}
