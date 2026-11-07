//! Parser for `origin-version.toml`, shared by `build.rs` and the unit tests in `src/lib.rs`.
//!
//! The accepted syntax is a strict subset of TOML: comments, blank lines, `[section]`
//! headers, and one `key = value` per line, where a value is a `"string"` without escapes
//! or a non-negative decimal integer without leading zeros. Anything else is an error, and
//! so are unknown sections, unknown keys, duplicates and missing keys. One file therefore
//! has exactly one reading, in the spirit of ONBF-R6.

/// A SemVer 2.0 version without build metadata.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct SemVer {
    pub(crate) major: u64,
    pub(crate) minor: u64,
    pub(crate) patch: u64,
    /// Pre-release identifiers after `-`, joined by `.`; empty for a release version.
    pub(crate) pre: String,
}

impl SemVer {
    /// The canonical string form, e.g. `0.1.0-dev`.
    pub(crate) fn text(&self) -> String {
        let core = format!("{}.{}.{}", self.major, self.minor, self.patch);
        if self.pre.is_empty() {
            core
        } else {
            format!("{core}-{}", self.pre)
        }
    }
}

/// Everything `origin-version.toml` declares.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Manifest {
    pub(crate) version: SemVer,
    pub(crate) build: u64,
    pub(crate) security_version: u32,
    pub(crate) onbf_format: (u16, u16),
}

enum Value<'a> {
    Str(&'a str),
    Int(u64),
}

/// The keys each section must contain, exactly once.
const SCHEMA: &[(&str, &[&str])] = &[
    ("release", &["version", "build", "security-version"]),
    ("compatibility", &["onbf-format"]),
];

/// Parses and validates the whole file. Errors name the 1-based line.
pub(crate) fn parse(text: &str) -> Result<Manifest, String> {
    let mut section: Option<&str> = None;
    let mut seen_sections: Vec<&str> = Vec::new();
    let mut entries: Vec<(&str, &str, Value<'_>, usize)> = Vec::new();

    for (index, raw) in text.lines().enumerate() {
        let line_no = index + 1;
        let err = |msg: String| format!("origin-version.toml:{line_no}: {msg}");
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(rest) = line.strip_prefix('[') {
            let (name, after) = rest
                .split_once(']')
                .ok_or_else(|| err("unterminated section header".into()))?;
            expect_end(after).map_err(err)?;
            if !SCHEMA.iter().any(|(s, _)| *s == name) {
                return Err(err(format!("unknown section [{name}]")));
            }
            if seen_sections.contains(&name) {
                return Err(err(format!("duplicate section [{name}]")));
            }
            seen_sections.push(name);
            section = Some(name);
            continue;
        }
        let (key, rest) = line
            .split_once('=')
            .ok_or_else(|| err("expected `key = value`".into()))?;
        let key = key.trim();
        if key.is_empty()
            || !key
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            return Err(err(format!("invalid key {key:?}")));
        }
        let sec = section.ok_or_else(|| err(format!("key `{key}` is outside any section")))?;
        let allowed = SCHEMA
            .iter()
            .find(|(s, _)| *s == sec)
            .map_or(&[][..], |(_, k)| *k);
        if !allowed.contains(&key) {
            return Err(err(format!("unknown key `{key}` in [{sec}]")));
        }
        if entries.iter().any(|(s, k, _, _)| *s == sec && *k == key) {
            return Err(err(format!("duplicate key `{key}` in [{sec}]")));
        }
        let value = parse_value(rest.trim_start()).map_err(err)?;
        entries.push((sec, key, value, line_no));
    }

    let get = |sec: &str, key: &str| -> Result<(&Value<'_>, usize), String> {
        entries
            .iter()
            .find(|(s, k, _, _)| *s == sec && *k == key)
            .map(|(_, _, v, line)| (v, *line))
            .ok_or_else(|| format!("origin-version.toml: missing key `{key}` in [{sec}]"))
    };
    let string = |sec: &str, key: &str| -> Result<(&str, usize), String> {
        match get(sec, key)? {
            (Value::Str(s), line) => Ok((s, line)),
            (_, line) => Err(format!(
                "origin-version.toml:{line}: `{key}` must be a string"
            )),
        }
    };
    let integer = |sec: &str, key: &str| -> Result<(u64, usize), String> {
        match get(sec, key)? {
            (Value::Int(n), line) => Ok((*n, line)),
            (_, line) => Err(format!(
                "origin-version.toml:{line}: `{key}` must be an integer"
            )),
        }
    };

    let (v, line) = string("release", "version")?;
    let version =
        parse_semver(v).map_err(|e| format!("origin-version.toml:{line}: version {v:?}: {e}"))?;
    let (build, _) = integer("release", "build")?;
    let (svn, line) = integer("release", "security-version")?;
    let security_version = u32::try_from(svn)
        .map_err(|_| format!("origin-version.toml:{line}: security-version must fit in 32 bits"))?;
    let (f, line) = string("compatibility", "onbf-format")?;
    let onbf_format = parse_major_minor(f)
        .map_err(|e| format!("origin-version.toml:{line}: onbf-format {f:?}: {e}"))?;

    Ok(Manifest {
        version,
        build,
        security_version,
        onbf_format,
    })
}

/// After a value or a section header, only whitespace and an optional comment may follow.
fn expect_end(rest: &str) -> Result<(), String> {
    let rest = rest.trim_start();
    if rest.is_empty() || rest.starts_with('#') {
        Ok(())
    } else {
        Err(format!("unexpected text {rest:?}"))
    }
}

fn parse_value(s: &str) -> Result<Value<'_>, String> {
    if let Some(body) = s.strip_prefix('"') {
        let (content, after) = body.split_once('"').ok_or("unterminated string")?;
        if content.contains('\\') {
            return Err("escape sequences are not allowed".into());
        }
        expect_end(after)?;
        return Ok(Value::Str(content));
    }
    let end = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
    let (digits, after) = s.split_at(end);
    let n = parse_decimal(digits)?;
    expect_end(after)?;
    Ok(Value::Int(n))
}

/// A non-negative decimal integer without sign, separators or leading zeros.
fn parse_decimal(s: &str) -> Result<u64, String> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return Err(format!(
            "expected a string or a non-negative integer, found {s:?}"
        ));
    }
    if s.len() > 1 && s.starts_with('0') {
        return Err(format!("leading zero in {s:?}"));
    }
    s.parse::<u64>()
        .map_err(|_| format!("{s:?} does not fit in 64 bits"))
}

/// SemVer 2.0 (semver.org), rejecting build metadata.
fn parse_semver(s: &str) -> Result<SemVer, String> {
    if s.contains('+') {
        return Err("build metadata (`+...`) is not allowed; use `build`".into());
    }
    let (core, pre) = match s.split_once('-') {
        Some((core, pre)) => (core, Some(pre)),
        None => (s, None),
    };
    let parts: Vec<&str> = core.split('.').collect();
    let [major, minor, patch] = parts.as_slice() else {
        return Err("expected MAJOR.MINOR.PATCH".into());
    };
    let pre = match pre {
        None => String::new(),
        Some(pre) => {
            for id in pre.split('.') {
                if id.is_empty() {
                    return Err("empty pre-release identifier".into());
                }
                if !id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
                    return Err(format!(
                        "invalid character in pre-release identifier {id:?}"
                    ));
                }
                if id.bytes().all(|b| b.is_ascii_digit()) && id.len() > 1 && id.starts_with('0') {
                    return Err(format!(
                        "leading zero in numeric pre-release identifier {id:?}"
                    ));
                }
            }
            pre.to_string()
        }
    };
    Ok(SemVer {
        major: parse_decimal(major)?,
        minor: parse_decimal(minor)?,
        patch: parse_decimal(patch)?,
        pre,
    })
}

/// `MAJOR.MINOR` with 16-bit parts, as in the ONBF header.
fn parse_major_minor(s: &str) -> Result<(u16, u16), String> {
    let (major, minor) = s.split_once('.').ok_or("expected MAJOR.MINOR")?;
    let to_u16 = |p: &str| -> Result<u16, String> {
        u16::try_from(parse_decimal(p)?).map_err(|_| format!("{p:?} does not fit in 16 bits"))
    };
    Ok((to_u16(major)?, to_u16(minor)?))
}
