//! Text generators. Everything here turns the validated model into files; none of it reads
//! the input again, so a rule enforced by `sema` needs no second check here.

pub mod c;
pub mod doc;
pub mod rust;

use crate::corpus;
use crate::fingerprint;
use crate::model::Compilation;

/// One generated file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Generated {
    /// Path relative to the output directory.
    pub path: String,
    /// Its text.
    pub text: String,
}

/// A text buffer with indentation, so generators read like the code they emit.
#[derive(Debug, Default)]
pub(crate) struct Text {
    text: String,
    depth: usize,
}

impl Text {
    pub(crate) fn line(&mut self, line: impl AsRef<str>) {
        let line = line.as_ref();
        if !line.is_empty() {
            self.text.push_str(&"    ".repeat(self.depth));
            self.text.push_str(line);
        }
        self.text.push('\n');
    }

    pub(crate) fn open(&mut self, line: impl AsRef<str>) {
        self.line(line);
        self.depth = self.depth.saturating_add(1);
    }

    pub(crate) fn shut(&mut self, line: impl AsRef<str>) {
        self.depth = self.depth.saturating_sub(1);
        self.line(line);
    }

    /// The closing brace of one block and the opening of the next: `} else {`.
    pub(crate) fn reopen(&mut self, line: impl AsRef<str>) {
        self.depth = self.depth.saturating_sub(1);
        self.line(line);
        self.depth = self.depth.saturating_add(1);
    }

    pub(crate) fn blank(&mut self) {
        self.text.push('\n');
    }

    /// A doc comment on the next item.
    pub(crate) fn doc(&mut self, lines: &[String]) {
        for line in lines {
            self.line(format!("/// {line}"));
        }
    }

    /// A doc comment in a C header. A block comment, because a line comment ending in a
    /// backslash would swallow the next line; the lexer refuses `*/` and `/*` in doc text.
    pub(crate) fn c_doc(&mut self, lines: &[String]) {
        for line in lines {
            self.line(format!("/** {line} */"));
        }
    }

    pub(crate) fn finish(self) -> String {
        self.text
    }
}

/// What [`generate_output`] writes (`docs/spec/OIDL.md` §11).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Output {
    /// Everything: what [`generate`] writes, and for each library its ABI fingerprint,
    /// `abi/<library>.fingerprint`, and its hostile-message corpus, `corpus/<library>.corpus`.
    Everything,
    /// The server side alone ([`generate_server`]).
    ServerOnly,
    /// One `channel` library's bindings crate ([`generate_library`]).
    Library(String),
}

/// The files of `output` for `comp`, sorted by path: what `oidl generate` writes, in the mode
/// its arguments name. The caller writes them; nothing here touches a file (ADR-0011
/// Amendment 4, T1).
///
/// # Errors
///
/// As [`generate_library`], for an unknown or `syscall` library.
pub fn generate_output(comp: &Compilation, output: &Output) -> Result<Vec<Generated>, String> {
    let mut files = match output {
        Output::ServerOnly => return Ok(generate_server(comp)),
        Output::Library(name) => return generate_library(comp, name),
        Output::Everything => generate(comp),
    };
    for library in &comp.libraries {
        files.push(Generated {
            path: format!("abi/{}.fingerprint", library.name),
            text: fingerprint::fingerprint_file(library),
        });
        files.push(Generated {
            path: format!("corpus/{}.corpus", library.name),
            text: corpus::corpus_for(library),
        });
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(files)
}

/// Everything the generators write, sorted by path.
pub fn generate(comp: &Compilation) -> Vec<Generated> {
    let mut files = rust::generate(comp);
    files.extend(c::generate(comp));
    files.extend(doc::generate(comp));
    files.sort_by(|a, b| a.path.cmp(&b.path));
    files
}

/// The server side alone (`docs/spec/OIDL.md` §11), sorted by path.
pub fn generate_server(comp: &Compilation) -> Vec<Generated> {
    let mut files = rust::generate_server(comp);
    files.sort_by(|a, b| a.path.cmp(&b.path));
    files
}

/// One `channel` library's bindings (`docs/spec/OIDL.md` §11), sorted by path; refused for an
/// unknown or `syscall` library.
pub fn generate_library(comp: &Compilation, name: &str) -> Result<Vec<Generated>, String> {
    let mut files = rust::generate_library(comp, name)?;
    files.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(files)
}
